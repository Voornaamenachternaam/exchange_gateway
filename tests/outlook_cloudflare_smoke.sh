# tests/outlook_cloudflare_smoke.sh
#!/usr/bin/env bash
set -euo pipefail

: "${GATEWAY_BASE_URL:?set GATEWAY_BASE_URL, e.g. https://mail.example.com}"
: "${GATEWAY_USER:?set GATEWAY_USER}"
: "${GATEWAY_PASS:?set GATEWAY_PASS}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

auth=(-u "${GATEWAY_USER}:${GATEWAY_PASS}")
base="${GATEWAY_BASE_URL%/}"
created_item_id=""
created_change_key=""

check_dns_dual_stack() {
  local host="$1"
  local a_record=""
  local aaaa_record=""
  if command -v getent >/dev/null 2>&1; then
    a_record="$(getent ahostsv4 "$host" | awk 'NR==1{print $1}')"
    aaaa_record="$(getent ahostsv6 "$host" | awk 'NR==1{print $1}')"
  fi
  if [[ -z "$a_record" || -z "$aaaa_record" ]]; then
    log "Warning: could not verify both A and AAAA via local resolver for ${host}."
  else
    log "Dual-stack DNS verified for ${host}: A=${a_record} AAAA=${aaaa_record}"
  fi
}

log() {
  printf '[smoke] %s\n' "$*"
}

require_contains() {
  local file="$1"
  local needle="$2"
  if ! grep -q "$needle" "$file"; then
    echo "Expected '$needle' in $file" >&2
    cat "$file" >&2
    exit 1
  fi
}

extract_item_id_and_change_key() {
  python3 - "$1" <<'PY'
import sys
import xml.etree.ElementTree as ET

root = ET.parse(sys.argv[1]).getroot()
for elem in root.iter():
    if elem.tag.endswith("ItemId"):
        item_id = elem.attrib.get("Id")
        change_key = elem.attrib.get("ChangeKey")
        if item_id and change_key:
            print(item_id)
            print(change_key)
            raise SystemExit(0)
raise SystemExit(1)
PY
}

request_xml() {
  local url="$1"
  local outfile="$2"
  local data="$3"
  shift 3 || true
  curl -fsS "${auth[@]}" \
    -H 'Content-Type: text/xml; charset=utf-8' \
    "$@" \
    --data "$data" \
    "$url" >"$outfile"
}


if [[ "${base}" =~ ^https?://([^/:]+) ]]; then
  check_dns_dual_stack "${BASH_REMATCH[1]}"
fi

log "Checking ActiveSync OPTIONS"
curl -fsSI "${auth[@]}" "${base}/Microsoft-Server-ActiveSync" >"${TMP_DIR}/options.txt"
require_contains "${TMP_DIR}/options.txt" "MS-ASProtocolVersions"

log "Checking ActiveSync 401 Bearer header (AutoDetect compatibility)"
# Unauthenticated request should return 401 with both Bearer and Basic.
# Per MS-XOAUTH §4.1, the Bearer header must include authorization_uri —
# without it, AutoDetect reports "missing authorization URL" and falls back to IMAP.
curl -sSI "${base}/Microsoft-Server-ActiveSync" >"${TMP_DIR}/eas-401.txt"
require_contains "${TMP_DIR}/eas-401.txt" "WWW-Authenticate: Bearer"
require_contains "${TMP_DIR}/eas-401.txt" "authorization_uri"
require_contains "${TMP_DIR}/eas-401.txt" "trusted_issuers"
require_contains "${TMP_DIR}/eas-401.txt" "WWW-Authenticate: Basic"

log "Checking Autodiscover XML"
curl -fsS \
  -H 'Content-Type: text/xml; charset=utf-8' \
  --data "<?xml version=\"1.0\"?><Autodiscover xmlns=\"http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006\"><Request><EMailAddress>${GATEWAY_USER}</EMailAddress><AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema></Request></Autodiscover>" \
  "${base}/autodiscover/autodiscover.xml" >"${TMP_DIR}/autodiscover.xml"
require_contains "${TMP_DIR}/autodiscover.xml" "<EwsUrl>${base}/EWS/Exchange.asmx</EwsUrl>"
require_contains "${TMP_DIR}/autodiscover.xml" "<ASUrl>${base}/Microsoft-Server-ActiveSync</ASUrl>"

log "Checking Autodiscover SOAP"
curl -fsS \
  -H 'Content-Type: application/soap+xml; charset=utf-8' \
  --data "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" xmlns:a=\"http://schemas.microsoft.com/exchange/2010/Autodiscover\"><s:Body><a:GetUserSettingsRequestMessage><a:Request><a:Users><a:User><a:Mailbox>${GATEWAY_USER}</a:Mailbox></a:User></a:Users><a:RequestedSettings><a:Setting>ExternalEwsUrl</a:Setting><a:Setting>MobileSyncServer</a:Setting></a:RequestedSettings></a:Request></a:GetUserSettingsRequestMessage></s:Body></s:Envelope>" \
  "${base}/autodiscover/autodiscover.svc" >"${TMP_DIR}/autodiscover.soap.xml"
require_contains "${TMP_DIR}/autodiscover.soap.xml" "ExternalEwsUrl"
require_contains "${TMP_DIR}/autodiscover.soap.xml" "MobileSyncServer"

log "Checking Autodiscover JSON (Protocol=ActiveSync, as requested by Outlook Android)"
curl -fsS "${base}/autodiscover/autodiscover.json?Protocol=ActiveSync" >"${TMP_DIR}/autodiscover.json"
require_contains "${TMP_DIR}/autodiscover.json" "\"Protocol\":\"ActiveSync\""
require_contains "${TMP_DIR}/autodiscover.json" "\"Url\":\"${base}/Microsoft-Server-ActiveSync\""

log "Checking Autodiscover JSON v1.0 path form with explicit email"
curl -fsS "${base}/autodiscover/autodiscover.json/v1.0/${GATEWAY_USER}?Protocol=ActiveSync" >"${TMP_DIR}/autodiscover.v1path.json"
require_contains "${TMP_DIR}/autodiscover.v1path.json" "\"Protocol\":\"ActiveSync\""
require_contains "${TMP_DIR}/autodiscover.v1path.json" "\"Url\":\"${base}/Microsoft-Server-ActiveSync\""

log "Checking Autodiscover JSON rejects missing/unknown Protocol (real Exchange V2 behaviour)"
status="$(curl -sS -o "${TMP_DIR}/autodiscover-missing.json" -w '%{http_code}' "${base}/autodiscover/autodiscover.json")"
[ "${status}" = "400" ] || { echo "FAIL: missing Protocol returned HTTP ${status}, expected 400" >&2; exit 1; }
require_contains "${TMP_DIR}/autodiscover-missing.json" "\"code\":\"MissingProtocol\""
status="$(curl -sS -o "${TMP_DIR}/autodiscover-unknown.json" -w '%{http_code}' "${base}/autodiscover/autodiscover.json?Protocol=Substrate")"
[ "${status}" = "400" ] || { echo "FAIL: unknown Protocol returned HTTP ${status}, expected 400" >&2; exit 1; }
require_contains "${TMP_DIR}/autodiscover-unknown.json" "\"code\":\"InvalidProtocol\""
status="$(curl -sS -o "${TMP_DIR}/autodiscover-rest.json" -w '%{http_code}' "${base}/autodiscover/autodiscover.json?Protocol=Rest")"
[ "${status}" = "400" ] || { echo "FAIL: Rest Protocol returned HTTP ${status}, expected 400 (no REST endpoint)" >&2; exit 1; }

log "Checking Autodiscover XML mobilesync schema (Outlook Android Exchange-account flow)"
curl -fsS \
  -H 'Content-Type: text/xml; charset=utf-8' \
  --data "<?xml version=\"1.0\"?><Autodiscover xmlns=\"http://schemas.microsoft.com/exchange/autodiscover/mobilesync/requestschema/2006\"><Request><EMailAddress>${GATEWAY_USER}</EMailAddress><AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006</AcceptableResponseSchema></Request></Autodiscover>" \
  "${base}/autodiscover/autodiscover.xml" >"${TMP_DIR}/autodiscover-mobilesync.xml"
require_contains "${TMP_DIR}/autodiscover-mobilesync.xml" "mobilesync/responseschema/2006"
require_contains "${TMP_DIR}/autodiscover-mobilesync.xml" "<Url>${base}/Microsoft-Server-ActiveSync</Url>"
require_contains "${TMP_DIR}/autodiscover-mobilesync.xml" "<Type>MobileSync</Type>"

log "Checking ActiveSync FolderSync bootstrap"
curl -fsS "${auth[@]}" \
  -H 'Content-Type: application/xml; charset=utf-8' \
  --data "<?xml version=\"1.0\" encoding=\"utf-8\"?><FolderSync xmlns=\"FolderHierarchy:\"><SyncKey>0</SyncKey></FolderSync>" \
  "${base}/Microsoft-Server-ActiveSync?Cmd=FolderSync&User=${GATEWAY_USER}&DeviceId=smoke-device&DeviceType=Outlook" >"${TMP_DIR}/foldersync.xml"
require_contains "${TMP_DIR}/foldersync.xml" "<Status>1</Status>"
require_contains "${TMP_DIR}/foldersync.xml" "<DisplayName>Calendar</DisplayName>"

log "Checking ActiveSync invalid SyncKey handling"
curl -fsS "${auth[@]}" \
  -H 'Content-Type: application/xml; charset=utf-8' \
  --data "<?xml version=\"1.0\" encoding=\"utf-8\"?><Sync xmlns=\"AirSync:\"><Collections><Collection><Class>Calendar</Class><SyncKey>bogus</SyncKey><CollectionId>1</CollectionId></Collection></Collections></Sync>" \
  "${base}/Microsoft-Server-ActiveSync?Cmd=Sync&User=${GATEWAY_USER}&DeviceId=smoke-device&DeviceType=Outlook" >"${TMP_DIR}/sync-invalid.xml"
require_contains "${TMP_DIR}/sync-invalid.xml" "<Status>9</Status>"

# ---------------------------------------------------------------------------
# Audit item 9: S/MIME certificates in the GAL (MS-ASCMD ResolveRecipients).
# ---------------------------------------------------------------------------
log "Checking EAS ResolveRecipients without CertificateRetrieval emits no Certificates block"
curl -fsS "${auth[@]}" \
  -H 'Content-Type: application/xml; charset=utf-8' \
  --data "<?xml version=\"1.0\" encoding=\"utf-8\"?><ResolveRecipients xmlns=\"ResolveRecipients:\"><To>${GATEWAY_USER}</To></ResolveRecipients>" \
  "${base}/Microsoft-Server-ActiveSync?Cmd=ResolveRecipients&User=${GATEWAY_USER}&DeviceId=smoke-device&DeviceType=Outlook" >"${TMP_DIR}/rr-nocert.xml"
require_contains "${TMP_DIR}/rr-nocert.xml" "<Status>1</Status>"
require_contains "${TMP_DIR}/rr-nocert.xml" "<EmailAddress>"

log "Checking EAS ResolveRecipients with CertificateRetrieval=2 (S/MIME GAL certificates)"
curl -fsS "${auth[@]}" \
  -H 'Content-Type: application/xml; charset=utf-8' \
  --data "<?xml version=\"1.0\" encoding=\"utf-8\"?><ResolveRecipients xmlns=\"ResolveRecipients:\"><To>${GATEWAY_USER}</To><Options><CertificateRetrieval>2</CertificateRetrieval></Options></ResolveRecipients>" \
  "${base}/Microsoft-Server-ActiveSync?Cmd=ResolveRecipients&User=${GATEWAY_USER}&DeviceId=smoke-device&DeviceType=Outlook" >"${TMP_DIR}/rr-cert.xml"
require_contains "${TMP_DIR}/rr-cert.xml" "<Status>1</Status>"
require_contains "${TMP_DIR}/rr-cert.xml" "<Certificates>"

log "Checking EAS ResolveRecipients rejects invalid CertificateRetrieval (MS-ASCMD Status 5)"
curl -fsS "${auth[@]}" \
  -H 'Content-Type: application/xml; charset=utf-8' \
  --data "<?xml version=\"1.0\" encoding=\"utf-8\"?><ResolveRecipients xmlns=\"ResolveRecipients:\"><To>${GATEWAY_USER}</To><Options><CertificateRetrieval>9</CertificateRetrieval></Options></ResolveRecipients>" \
  "${base}/Microsoft-Server-ActiveSync?Cmd=ResolveRecipients&User=${GATEWAY_USER}&DeviceId=smoke-device&DeviceType=Outlook" >"${TMP_DIR}/rr-badopt.xml"
require_contains "${TMP_DIR}/rr-badopt.xml" "<Status>5</Status>"

log "Checking EAS ResolveRecipients WBXML wire format carries certificates (real Outlook Android form)"
python3 - "$TMP_DIR" <<'PY'
"""Encode a ResolveRecipients request in WBXML and POST it to the EAS
endpoint, then assert the WBXML response contains the code-page-10
Certificates/Status tokens."""
import struct, subprocess, sys

tmp = sys.argv[1]

def str_i(s: bytes) -> bytes:
    return b"\x03" + s + b"\x00"

# Code page 10 (ResolveRecipients) tags per MS-ASWBXML.
def t(code: int, content: bytes = b"") -> bytes:
    if content:
        return bytes([0x40 | code]) + content + b"\x01"
    return bytes([0x40 | code, 0x01])

wbxml = (
    b"\x03\x01\x6a\x00"          # header: WBXML 1.3, UTF-8, str table len 0
    b"\x00\x0a"                  # SWITCH_PAGE 10 (ResolveRecipients)
    + t(0x05,                    # ResolveRecipients
        t(0x10, str_i(b"smoke")) # To
        + t(0x0F,                # Options
            t(0x11, str_i(b"2")) # CertificateRetrieval = 2 (full certs)
        )
    )
)

import urllib.request
req = urllib.request.Request(
    __import__("os").environ["GATEWAY_BASE_URL"].rstrip("/")
    + "/Microsoft-Server-ActiveSync?Cmd=ResolveRecipients&User="
    + __import__("os").environ["GATEWAY_USER"]
    + "&DeviceId=smoke-device&DeviceType=Outlook",
    data=wbxml,
    method="POST",
)
import base64
cred = base64.b64encode(
    (__import__("os").environ["GATEWAY_USER"] + ":" + __import__("os").environ["GATEWAY_PASS"]).encode()
).decode()
req.add_header("Authorization", "Basic " + cred)
req.add_header("Content-Type", "application/vnd.ms-sync")
req.add_header("MS-ASProtocolVersion", "16.1")
with urllib.request.urlopen(req) as resp:
    body = resp.read()

if body[:4] != b"\x03\x01\x6a\x00":
    raise SystemExit("FAIL: ResolveRecipients response is not WBXML")
# After the header, expect SWITCH_PAGE 10 and the ResolveRecipients tag.
if body[4:6] != b"\x00\x0a" or (body[6] & 0x3F) != 0x05:
    raise SystemExit(f"FAIL: unexpected WBXML token stream: {body[:12].hex()}")
if (0x0C | 0x40) not in body:
    raise SystemExit("FAIL: Certificates tag (cp10/0x0C) missing in WBXML response")
print("[smoke] WBXML ResolveRecipients response carries Certificates on code page 10")
PY

log "Checking EWS GetFolder"
request_xml \
  "${base}/EWS/Exchange.asmx" \
  "${TMP_DIR}/getfolder.xml" \
  "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:GetFolder xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\" xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\"><m:FolderShape><t:BaseShape>Default</t:BaseShape></m:FolderShape><m:FolderIds><t:DistinguishedFolderId Id=\"calendar\"/></m:FolderIds></m:GetFolder></s:Body></s:Envelope>"
require_contains "${TMP_DIR}/getfolder.xml" "CalendarFolder"

log "Checking EWS GetUserAvailability"
request_xml \
  "${base}/EWS/Exchange.asmx" \
  "${TMP_DIR}/availability.xml" \
  "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:GetUserAvailabilityRequest xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\" xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\"><t:TimeZone><t:Bias>0</t:Bias><t:StandardTime><t:Bias>0</t:Bias><t:Time>02:00:00</t:Time><t:DayOrder>1</t:DayOrder><t:Month>11</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek></t:StandardTime><t:DaylightTime><t:Bias>-60</t:Bias><t:Time>02:00:00</t:Time><t:DayOrder>2</t:DayOrder><t:Month>3</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek></t:DaylightTime></t:TimeZone><m:MailboxDataArray><t:MailboxData><t:Email><t:Address>${GATEWAY_USER}</t:Address></t:Email><t:AttendeeType>Required</t:AttendeeType><t:ExcludeConflicts>false</t:ExcludeConflicts></t:MailboxData></m:MailboxDataArray><t:FreeBusyViewOptions><t:TimeWindow><t:StartTime>2026-03-22T00:00:00Z</t:StartTime><t:EndTime>2026-03-29T00:00:00Z</t:EndTime></t:TimeWindow><t:MergedFreeBusyIntervalInMinutes>30</t:MergedFreeBusyIntervalInMinutes><t:RequestedView>DetailedMerged</t:RequestedView></t:FreeBusyViewOptions></m:GetUserAvailabilityRequest></s:Body></s:Envelope>"
require_contains "${TMP_DIR}/availability.xml" "MergedFreeBusy"
require_contains "${TMP_DIR}/availability.xml" "CalendarEventArray"

# ---------------------------------------------------------------------------
# Audit item 5: verify JMAP Calendars against Stalwart v0.16.22 at deploy time.
#
# When STALWART_JMAP_URL is set (e.g. http://stalwart:8080/jmap), this checks
# the backend directly — the capability the gateway itself validates at
# startup via GATEWAY_CALENDAR_CAPABILITY_CHECK — and performs a real
# CalendarEvent/set write round-trip with a recurring event plus a per-instance
# exception (the shape EAS MS-ASCAL <Exceptions> relies on), then destroys it.
# ---------------------------------------------------------------------------
if [[ -n "${STALWART_JMAP_URL:-}" ]]; then
  log "Checking Stalwart JMAP session advertises urn:ietf:params:jmap:calendars"
  stalwart_auth=(-u "${STALWART_USER:-${GATEWAY_USER}}:${STALWART_PASS:-${GATEWAY_PASS}}")
  jmap_base="${STALWART_JMAP_URL%/}"
  curl -fsS "${stalwart_auth[@]}" "${jmap_base}/session" >"${TMP_DIR}/jmap-session.json"
  require_contains "${TMP_DIR}/jmap-session.json" '"urn:ietf:params:jmap:calendars"'

  log "Running CalendarEvent/set recurring+exception round-trip against Stalwart"
  # Credentials go via the environment, never the argument vector: argv is
  # world-readable through ps and /proc/<pid>/cmdline (CWE-214).
  SMOKE_JMAP_USER="${STALWART_USER:-${GATEWAY_USER}}" \
  SMOKE_JMAP_PASS="${STALWART_PASS:-${GATEWAY_PASS}}" \
  python3 - "$jmap_base" "${TMP_DIR}/jmap-session.json" <<'PY'
import base64, json, os, sys, urllib.request

jmap_base, session_file = sys.argv[1:3]
user = os.environ["SMOKE_JMAP_USER"]
password = os.environ["SMOKE_JMAP_PASS"]
session = json.load(open(session_file))
cap = "urn:ietf:params:jmap:calendars"
account = session["primaryAccounts"][cap]
api_url = session["apiUrl"]
if api_url.startswith("http://") or api_url.startswith("https://"):
    pass
else:
    api_url = jmap_base.rsplit("/", 1)[0] + api_url
auth = base64.b64encode(f"{user}:{password}".encode()).decode()

def call(using, method_calls):
    req = urllib.request.Request(
        api_url,
        data=json.dumps({"using": using, "methodCalls": method_calls}).encode(),
        headers={"Authorization": f"Basic {auth}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)

# Find any calendar in the account.
resp = call(["urn:ietf:params:jmap:core", cap],
            [["Calendar/get", {"accountId": account, "ids": None}, "g"]])
cal_list = resp["methodResponses"][0][1]["list"]
if not cal_list:
    raise SystemExit("no calendars returned by Calendar/get")
calendar_id = cal_list[0]["id"]

# Create a weekly recurring event with one excluded instance (exception).
# Verified live against stalwartlabs/stalwart:v0.16.22: RFC-form
# `recurrenceRules` arrays are rejected; this build speaks the older-draft
# spelling `recurrenceRule` (single object) + `recurrenceOverrides`.
# Unique UID per run: Stalwart rejects a create whose UID collides with an
# event orphaned by an earlier failed cleanup, which would break re-runs.
import uuid
uid = f"jmap-cal-smoke-event-{uuid.uuid4()}"
create = {
    "accountId": account,
    "create": {
        "ev1": {
            "calendarIds": {calendar_id: True},
            "uid": uid,
            "title": "jmap-cal-smoke recurring",
            "start": "2026-03-02T09:00:00",
            "timeZone": "Europe/Berlin",
            "duration": "PT1H",
            "recurrenceRule": {"frequency": "weekly"},
            "recurrenceOverrides": {
                "2026-03-09T09:00:00": {"excluded": True}
            },
        }
    },
}
resp = call(["urn:ietf:params:jmap:core", cap], [["CalendarEvent/set", create, "s"]])
args = resp["methodResponses"][0][1]
# notCreated only signals failure when non-null and non-empty; servers may
# echo an explicit null or empty object on success.
nc = args.get("notCreated")
if nc is not None and (not isinstance(nc, dict) or nc):
    raise SystemExit(f"CalendarEvent/set create failed: {nc}")
event_id = args["created"]["ev1"]["id"]

try:
    # Read back and assert the recurrence and exception survive.
    resp = call(
        ["urn:ietf:params:jmap:core", cap],
        [["CalendarEvent/get",
          {"accountId": account, "ids": [event_id],
           "properties": ["uid", "recurrenceRule", "recurrenceOverrides", "timeZone"]},
          "r"]],
    )
    ev = resp["methodResponses"][0][1]["list"][0]
    assert ev["recurrenceRule"]["frequency"] == "weekly", ev
    assert ev["recurrenceOverrides"]["2026-03-09T09:00:00"]["excluded"] is True, ev
    # The event must expand into its first occurrence window.
    resp = call(
        ["urn:ietf:params:jmap:core", cap],
        [["CalendarEvent/query",
          {"accountId": account,
           "filter": {"after": "2026-03-02T00:00:00Z",
                      "before": "2026-03-03T00:00:00Z"}},
          "q"]],
    )
    ids = resp["methodResponses"][0][1].get("ids", [])
    assert event_id in ids, ids
    print("[smoke] recurrence + exception round-trip + occurrence expansion OK")
finally:
    call(["urn:ietf:params:jmap:core", cap],
         [["CalendarEvent/set",
           {"accountId": account, "destroy": [event_id]}, "d"]])
PY

  log "Stalwart JMAP calendars capability verified"
fi

if [[ "${RUN_MUTATION_PROBE:-0}" == "1" ]]; then
  now_stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  subject="gateway-smoke-${now_stamp}"

  log "Running EWS CreateItem / UpdateItem / DeleteItem mutation probe"
  request_xml \
    "${base}/EWS/Exchange.asmx" \
    "${TMP_DIR}/create.xml" \
    "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:CreateItem xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\" xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\" SendMeetingInvitations=\"SendToNone\"><m:SavedItemFolderId><t:DistinguishedFolderId Id=\"calendar\"/></m:SavedItemFolderId><m:Items><t:CalendarItem><t:Subject>${subject}</t:Subject><t:Start>2026-03-22T12:00:00Z</t:Start><t:End>2026-03-22T13:00:00Z</t:End><t:IsAllDayEvent>false</t:IsAllDayEvent><t:LegacyFreeBusyStatus>Busy</t:LegacyFreeBusyStatus></t:CalendarItem></m:Items></m:CreateItem></s:Body></s:Envelope>"
  mapfile -t item_bits < <(extract_item_id_and_change_key "${TMP_DIR}/create.xml")
  [[ -n "${item_bits[0]:-}" && -n "${item_bits[1]:-}" ]] || { echo "Failed to extract ItemId/ChangeKey from create response" >&2; exit 1; }
  created_item_id="${item_bits[0]}"
  created_change_key="${item_bits[1]}"

  request_xml \
    "${base}/EWS/Exchange.asmx" \
    "${TMP_DIR}/update.xml" \
    "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:UpdateItem xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\" xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\" ConflictResolution=\"AutoResolve\" SendMeetingInvitationsOrCancellations=\"SendToNone\"><m:ItemChanges><t:ItemChange><t:ItemId Id=\"${created_item_id}\" ChangeKey=\"${created_change_key}\"/><t:Updates><t:SetItemField><t:FieldURI FieldURI=\"item:Subject\"/><t:CalendarItem><t:Subject>${subject}-updated</t:Subject></t:CalendarItem></t:SetItemField></t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem></s:Body></s:Envelope>"
  mapfile -t item_bits < <(extract_item_id_and_change_key "${TMP_DIR}/update.xml")
  created_change_key="${item_bits[1]}"

  request_xml \
    "${base}/EWS/Exchange.asmx" \
    "${TMP_DIR}/delete.xml" \
    "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:DeleteItem xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\" xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\" DeleteType=\"HardDelete\" SendMeetingCancellations=\"SendToNone\"><m:ItemIds><t:ItemId Id=\"${created_item_id}\" ChangeKey=\"${created_change_key}\"/></m:ItemIds></m:DeleteItem></s:Body></s:Envelope>"
  require_contains "${TMP_DIR}/delete.xml" "NoError"
fi

log "Smoke checks completed successfully"
