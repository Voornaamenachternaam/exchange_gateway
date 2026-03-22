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
import re, sys
text = open(sys.argv[1], 'r', encoding='utf-8').read()
m = re.search(r'ItemId Id="([^"]+)" ChangeKey="([^"]+)"', text)
if not m:
    raise SystemExit(1)
print(m.group(1))
print(m.group(2))
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

log "Checking ActiveSync OPTIONS"
curl -fsSI "${auth[@]}" "${base}/Microsoft-Server-ActiveSync" >"${TMP_DIR}/options.txt"
require_contains "${TMP_DIR}/options.txt" "MS-ASProtocolVersions"

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

if [[ "${RUN_MUTATION_PROBE:-0}" == "1" ]]; then
  now_stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  subject="gateway-smoke-${now_stamp}"

  log "Running EWS CreateItem / UpdateItem / DeleteItem mutation probe"
  request_xml \
    "${base}/EWS/Exchange.asmx" \
    "${TMP_DIR}/create.xml" \
    "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:CreateItem xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\" xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\" SendMeetingInvitations=\"SendToNone\"><m:SavedItemFolderId><t:DistinguishedFolderId Id=\"calendar\"/></m:SavedItemFolderId><m:Items><t:CalendarItem><t:Subject>${subject}</t:Subject><t:Start>2026-03-22T12:00:00Z</t:Start><t:End>2026-03-22T13:00:00Z</t:End><t:IsAllDayEvent>false</t:IsAllDayEvent><t:LegacyFreeBusyStatus>Busy</t:LegacyFreeBusyStatus></t:CalendarItem></m:Items></m:CreateItem></s:Body></s:Envelope>"
  mapfile -t item_bits < <(extract_item_id_and_change_key "${TMP_DIR}/create.xml")
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
