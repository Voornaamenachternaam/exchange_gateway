#!/usr/bin/env python3
"""
make_binder_dataset.py

Build a queryable SQLite dataset from a noisy UTF-8 export that contains one or
more VEVENT blocks plus surrounding XML / header text.

Outputs:
  - <stem>_dataset.sqlite
  - <stem>_events.jsonl
  - <stem>_all_blocks.jsonl

Usage:
  python3 make_binder_dataset.py Binder1_utf8.txt
"""

from __future__ import annotations

import json
import re
import sqlite3
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, Iterable, List, Tuple


VEVENT_START = re.compile(r"^BEGIN:VEVENT\s*$", re.IGNORECASE)
VEVENT_END = re.compile(r"^END:VEVENT\s*$", re.IGNORECASE)


@dataclass
class Block:
    block_type: str
    start_line: int
    end_line: int
    raw_text: str


def unfold_ical_lines(lines: List[str]) -> List[str]:
    """Unfold RFC 5545 lines where a continuation begins with space or tab."""
    unfolded: List[str] = []
    for line in lines:
        if line.startswith((" ", "\t")) and unfolded:
            unfolded[-1] += line[1:]
        else:
            unfolded.append(line.rstrip("\r\n"))
    return unfolded


def split_prop_line(line: str) -> Tuple[str, Dict[str, str], str]:
    """
    Parse a single iCalendar property line:
        NAME;PARAM=VALUE:property value
    Returns (name, params, value).
    """
    if ":" not in line:
        return line.strip(), {}, ""
    left, value = line.split(":", 1)
    parts = left.split(";")
    name = parts[0].strip().upper()
    params: Dict[str, str] = {}
    for chunk in parts[1:]:
        if "=" in chunk:
            k, v = chunk.split("=", 1)
            params[k.strip().upper()] = v.strip()
        else:
            params[chunk.strip().upper()] = ""
    return name, params, value.strip()


def parse_vevent(raw_text: str) -> Dict[str, object]:
    """Extract common calendar fields and preserve the raw event text."""
    lines = unfold_ical_lines(raw_text.splitlines())
    props: Dict[str, List[str]] = {}
    parsed_lines: List[Dict[str, object]] = []

    for line in lines:
        if not line or line.upper() in {"BEGIN:VEVENT", "END:VEVENT"}:
            continue
        key, params, value = split_prop_line(line)
        parsed_lines.append({"key": key, "params": params, "value": value})
        props.setdefault(key, []).append(value)

    def first(*keys: str) -> str:
        for key in keys:
            if key in props and props[key]:
                return props[key][0]
        return ""

    return {
        "uid": first("UID"),
        "summary": first("SUMMARY"),
        "description": first("DESCRIPTION"),
        "location": first("LOCATION"),
        "dtstart": first("DTSTART"),
        "dtend": first("DTEND"),
        "duration": first("DURATION"),
        "status": first("STATUS"),
        "organizer": first("ORGANIZER"),
        "categories": "|".join(props.get("CATEGORIES", [])),
        "raw_text": raw_text,
        "parsed_lines": parsed_lines,
    }


def extract_blocks(text: str) -> List[Block]:
    lines = text.splitlines()
    blocks: List[Block] = []

    in_event = False
    event_start = 0
    event_buf: List[str] = []

    non_event_start = 1
    non_event_buf: List[str] = []

    def flush_non_event(end_line: int) -> None:
        nonlocal non_event_buf, non_event_start
        raw = "\n".join(non_event_buf).strip("\n")
        if raw.strip():
            blocks.append(Block("text", non_event_start, end_line, raw))
        non_event_buf = []

    for idx, line in enumerate(lines, start=1):
        if VEVENT_START.match(line):
            if non_event_buf:
                flush_non_event(idx - 1)
            in_event = True
            event_start = idx
            event_buf = [line]
            continue

        if in_event:
            event_buf.append(line)
            if VEVENT_END.match(line):
                blocks.append(Block("vevent", event_start, idx, "\n".join(event_buf)))
                in_event = False
                non_event_start = idx + 1
                event_buf = []
            continue

        if not non_event_buf:
            non_event_start = idx
        non_event_buf.append(line)

    if in_event and event_buf:
        blocks.append(Block("vevent_unclosed", event_start, len(lines), "\n".join(event_buf)))

    if non_event_buf:
        flush_non_event(len(lines))

    return blocks


def init_db(conn: sqlite3.Connection) -> None:
    conn.execute("PRAGMA journal_mode=WAL;")
    conn.execute("PRAGMA synchronous=NORMAL;")
    conn.execute("""
        CREATE TABLE IF NOT EXISTS blocks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            block_type TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            raw_text TEXT NOT NULL
        );
    """)
    conn.execute("""
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            block_id INTEGER NOT NULL,
            uid TEXT,
            summary TEXT,
            description TEXT,
            location TEXT,
            dtstart TEXT,
            dtend TEXT,
            duration TEXT,
            status TEXT,
            organizer TEXT,
            categories TEXT,
            raw_text TEXT NOT NULL,
            parsed_json TEXT NOT NULL,
            FOREIGN KEY(block_id) REFERENCES blocks(id)
        );
    """)
    conn.execute("""
        CREATE VIRTUAL TABLE IF NOT EXISTS blocks_fts
        USING fts5(raw_text, content='blocks', content_rowid='id');
    """)
    conn.execute("""
        CREATE VIRTUAL TABLE IF NOT EXISTS events_fts
        USING fts5(summary, description, location, raw_text, content='events', content_rowid='id');
    """)


def populate_db(conn: sqlite3.Connection, blocks: List[Block]) -> List[Dict[str, object]]:
    event_rows: List[Dict[str, object]] = []

    for block in blocks:
        cur = conn.execute(
            "INSERT INTO blocks(block_type, start_line, end_line, raw_text) VALUES (?, ?, ?, ?)",
            (block.block_type, block.start_line, block.end_line, block.raw_text),
        )
        block_id = cur.lastrowid

        conn.execute(
            "INSERT INTO blocks_fts(rowid, raw_text) VALUES (?, ?)",
            (block_id, block.raw_text),
        )

        if block.block_type.startswith("vevent"):
            event = parse_vevent(block.raw_text)
            parsed_json = json.dumps(event["parsed_lines"], ensure_ascii=False, indent=2)
            cur2 = conn.execute(
                """
                INSERT INTO events(
                    block_id, uid, summary, description, location,
                    dtstart, dtend, duration, status, organizer,
                    categories, raw_text, parsed_json
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    block_id,
                    event["uid"],
                    event["summary"],
                    event["description"],
                    event["location"],
                    event["dtstart"],
                    event["dtend"],
                    event["duration"],
                    event["status"],
                    event["organizer"],
                    event["categories"],
                    event["raw_text"],
                    parsed_json,
                ),
            )
            event_id = cur2.lastrowid
            conn.execute(
                "INSERT INTO events_fts(rowid, summary, description, location, raw_text) VALUES (?, ?, ?, ?, ?)",
                (
                    event_id,
                    event["summary"],
                    event["description"],
                    event["location"],
                    event["raw_text"],
                ),
            )
            event_rows.append({
                "id": event_id,
                **{k: event[k] for k in [
                    "uid", "summary", "description", "location", "dtstart",
                    "dtend", "duration", "status", "organizer", "categories"
                ]},
                "block_id": block_id,
            })

    conn.commit()
    return event_rows


def main() -> int:
    if len(sys.argv) != 2:
        print("Usage: python3 make_binder_dataset.py Binder1_utf8.txt", file=sys.stderr)
        return 2

    input_path = Path(sys.argv[1]).expanduser().resolve()
    if not input_path.exists():
        print(f"Input file not found: {input_path}", file=sys.stderr)
        return 2

    text = input_path.read_text(encoding="utf-8", errors="strict")
    blocks = extract_blocks(text)

    out_sqlite = input_path.with_name(f"{input_path.stem}_dataset.sqlite")
    out_events = input_path.with_name(f"{input_path.stem}_events.jsonl")
    out_blocks = input_path.with_name(f"{input_path.stem}_all_blocks.jsonl")

    conn = sqlite3.connect(out_sqlite)
    try:
        init_db(conn)
        event_rows = populate_db(conn, blocks)
    finally:
        conn.close()

    with out_events.open("w", encoding="utf-8") as f:
        for row in event_rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")

    with out_blocks.open("w", encoding="utf-8") as f:
        for i, block in enumerate(blocks, start=1):
            f.write(json.dumps({
                "id": i,
                "block_type": block.block_type,
                "start_line": block.start_line,
                "end_line": block.end_line,
                "raw_text": block.raw_text,
            }, ensure_ascii=False) + "\n")

    print(f"Wrote: {out_sqlite}")
    print(f"Wrote: {out_events}")
    print(f"Wrote: {out_blocks}")
    print(f"Blocks found: {len(blocks)}")
    print(f"VEVENT rows: {len(event_rows)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
