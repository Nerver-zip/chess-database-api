#!/usr/bin/env python3
"""Audit Masters broadcast events against event-level tournament evidence.

The audit consumes accepted PGN exported from a Masters release, groups repeated
games by Lichess broadcast tournament, and records official Broadcast API
metadata plus linked event-page evidence. It has no third-party dependencies.
"""

from __future__ import annotations

import argparse
import hashlib
import html.parser
import ipaddress
import json
import re
import socket
import sys
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import quote, unquote, urljoin, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener, urlopen

TAG_RE = re.compile(r'^\[([A-Za-z0-9_]+)\s+"((?:\\.|[^"\\])*)"\]$')
ONLINE_TEXT = re.compile(r"\bonline\b|\binternet\b|\bengine\b", re.IGNORECASE)
STOP_WORDS = {"the", "and", "open", "round", "matches", "tournament", "chess", "cup"}


def unescape_tag(value: str) -> str:
    return re.sub(r'\\(["\\])', r'\1', value)


def iter_pgn_headers(text: str):
    current: dict[str, str] = {}
    for line in text.splitlines():
        if line.startswith("[Event "):
            if current:
                yield current
            current = {}
        match = TAG_RE.match(line.strip())
        if match:
            current[match.group(1)] = unescape_tag(match.group(2))
    if current:
        yield current


def broadcast_parts(value: str) -> tuple[str | None, list[str]]:
    parsed = urlsplit(value)
    parts = [unquote(part) for part in parsed.path.split("/") if part]
    try:
        index = parts.index("broadcast")
    except ValueError:
        return None, []
    tail = parts[index + 1 :]
    return (tail[0].casefold(), tail) if tail else (None, [])


def metadata_endpoint(tail: list[str]) -> str:
    return "https://lichess.org/api/broadcast/" + "/".join(
        quote(part, safe="-_.") for part in tail
    )


def fetch_event(api_url: str) -> dict:
    request = Request(
        api_url,
        headers={
            "Accept": "application/json",
            "User-Agent": "opening-explorer-api-masters-event-audit/1.0",
        },
    )
    for attempt in range(3):
        try:
            with urlopen(request, timeout=20) as response:
                payload = json.loads(response.read(8 * 1024 * 1024 + 1))
            break
        except HTTPError as error:
            if error.code != 429 or attempt == 2:
                raise
            time.sleep(60)
    tour = payload.get("tour") if isinstance(payload, dict) else None
    if not isinstance(tour, dict):
        raise ValueError("Broadcast API response did not contain tournament metadata")
    info = tour.get("info") if isinstance(tour.get("info"), dict) else {}
    return {
        "tour": {
            "id": tour.get("id"),
            "name": tour.get("name"),
            "url": tour.get("url"),
            "tier": tour.get("tier"),
            "info": {
                key: info.get(key)
                for key in ("format", "tc", "fideTC", "location", "timeZone", "website", "standings")
                if info.get(key) is not None
            },
        }
    }


class VisibleTextParser(html.parser.HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.parts: list[str] = []
        self.hidden_depth = 0

    def handle_starttag(self, tag, attrs):
        if tag.casefold() in ("script", "style", "noscript"):
            self.hidden_depth += 1

    def handle_endtag(self, tag):
        if tag.casefold() in ("script", "style", "noscript") and self.hidden_depth:
            self.hidden_depth -= 1

    def handle_data(self, data):
        if not self.hidden_depth:
            self.parts.append(data)


def normalize_words(value: str) -> set[str]:
    return {
        token.casefold()
        for token in re.findall(r"[^\W_]+", value, flags=re.UNICODE)
        if len(token) >= 4 and token.casefold() not in STOP_WORDS
    }


def is_public_http_url(url: str) -> bool:
    try:
        parsed = urlsplit(url)
        if (parsed.scheme not in ("http", "https") or not parsed.hostname
                or parsed.username is not None or parsed.password is not None):
            return False
        port = parsed.port or (443 if parsed.scheme == "https" else 80)
        addresses = socket.getaddrinfo(parsed.hostname, port)
        return bool(addresses) and all(
            ipaddress.ip_address(item[4][0].split("%", 1)[0]).is_global
            for item in addresses
        )
    except (OSError, ValueError):
        return False


class PublicRedirectHandler(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        target = urljoin(request.full_url, new_url)
        if not is_public_http_url(target):
            return None
        return super().redirect_request(request, response, code, message, headers, target)


def linked_page_evidence(url: str, event_names: list[str], tour_name: str,
                         location: str) -> dict:
    if not is_public_http_url(url):
        return {"url": url, "verified": False,
                "error": "host is invalid, unresolved, or not a public HTTP destination"}

    request = Request(url, headers={"User-Agent": "opening-explorer-api-masters-event-audit/1.0"})
    opener = build_opener(PublicRedirectHandler())
    try:
        with opener.open(request, timeout=1.5) as response:
            raw = response.read(4 * 1024 * 1024 + 1)
            final_url = response.geturl()
            status = response.status
        if len(raw) > 4 * 1024 * 1024:
            return {"url": url, "verified": False, "error": "linked page exceeds 4 MiB"}
        text = raw.decode("utf-8", errors="replace")
        parser = VisibleTextParser()
        parser.feed(text)
        page_text = " ".join(parser.parts)
        normalized = page_text.casefold()
        event_terms = normalize_words(" ".join([tour_name, *event_names]))
        location_terms = normalize_words(location)
        matched_events = sorted(term for term in event_terms if term in normalized)
        matched_locations = sorted(term for term in location_terms if term in normalized)
        verified = status == 200 and len(matched_events) >= 2 and bool(matched_locations)
        return {
            "url": url,
            "final_url": final_url,
            "http_status": status,
            "sha256": hashlib.sha256(raw).hexdigest(),
            "bytes": len(raw),
            "matched_event_terms": matched_events,
            "matched_location_terms": matched_locations,
            "verified": verified,
        }
    except (HTTPError, URLError, TimeoutError, OSError, ValueError) as exc:
        return {"url": url, "verified": False, "error": f"{type(exc).__name__}: {exc}"}


def is_online_metadata(slug: str, event_names: list[str], tour: dict,
                       sites: list[str] | tuple[str, ...] = (),
                       broadcast_urls: list[str] | tuple[str, ...] = ()) -> list[str]:
    info = tour.get("info", {})
    evidence: list[str] = []
    named_values = [
        ("tournament name", tour.get("name")),
        ("format", info.get("format")),
        ("location", info.get("location")),
        ("tournament identifier", slug),
        ("tournament website URL", info.get("website")),
    ]
    named_values.extend(("PGN Event tag", value) for value in event_names)
    named_values.extend(("PGN Site tag", value) for value in sites)
    named_values.extend(("PGN BroadcastURL", value) for value in broadcast_urls)
    for label, value in named_values:
        if isinstance(value, str) and ONLINE_TEXT.search(value):
            evidence.append(f"{label} explicitly identifies online play or computer engines: {value}")
    if not evidence and ("engine" in slug or slug.startswith("tcec")):
        evidence.append(f"broadcast tournament identifier identifies an engine event: {slug}")
    return evidence


def classify_event(slug: str, event_names: list[str], tour: dict | None,
                   external_evidence: dict | None = None,
                   sites: list[str] | tuple[str, ...] = (),
                   broadcast_urls: list[str] | tuple[str, ...] = ()) -> tuple[str, list[str]]:
    if not isinstance(tour, dict):
        return "unresolved", ["official broadcast tournament metadata is unavailable"]
    online_evidence = is_online_metadata(slug, event_names, tour, sites, broadcast_urls)
    if online_evidence:
        return "verified_online", online_evidence
    info = tour.get("info", {})
    location = info.get("location") if isinstance(info, dict) else None
    if isinstance(location, str) and location.strip() and external_evidence and external_evidence.get("verified"):
        return "verified_otb", [
            f"official broadcast metadata reports physical location {location.strip()}",
            "linked event source independently matches tournament identity and location: "
            + str(external_evidence.get("final_url") or external_evidence.get("url")),
        ]
    if isinstance(location, str) and location.strip():
        return "unresolved", [
            f"broadcast metadata reports location {location.strip()}, but a linked event source did not corroborate the event and venue"
        ]
    return "unresolved", [
        "official tournament metadata has no explicit online evidence or physical venue corroborated by a linked event source"
    ]


def canonical_json_hash(value: object) -> str:
    raw = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(raw).hexdigest()


def run_audit(pgn_path: Path, output_path: Path, cache_path: Path, *, fetch: bool, source_version: str | None = None) -> dict:
    grouped: dict[str, dict] = {}
    text = pgn_path.read_text(encoding="utf-8", errors="replace")
    for tags in iter_pgn_headers(text):
        broadcast_url = tags.get("BroadcastURL") or tags.get("BroadcastUrl") or ""
        slug, tail = broadcast_parts(broadcast_url) if broadcast_url else (None, [])
        event_name = tags.get("Event", "(missing Event)")
        site = tags.get("Site", "(missing Site)")
        if slug is None:
            slug = "unlinked-" + hashlib.sha256(
                (event_name.casefold() + "\0" + site.casefold()).encode()
            ).hexdigest()[:16]
        row = grouped.setdefault(slug, {
            "tournament_slug": slug if not slug.startswith("unlinked-") else None,
            "game_count": 0,
            "event_names": set(),
            "sites": set(),
            "broadcast_urls": set(),
            "api_tail": tail,
        })
        row["game_count"] += 1
        row["event_names"].add(event_name)
        row["sites"].add(site)
        if broadcast_url:
            row["broadcast_urls"].add(broadcast_url)
        if not row["api_tail"] and tail:
            row["api_tail"] = tail

    cache: dict = {"events": {}, "sites": {}}
    if cache_path.exists():
        loaded = json.loads(cache_path.read_text(encoding="utf-8"))
        if not isinstance(loaded, dict):
            raise ValueError(f"metadata cache must be a JSON object: {cache_path}")
        cache = {"events": loaded.get("events", {}), "sites": loaded.get("sites", {})}

    events = []
    for index, (slug, row) in enumerate(sorted(grouped.items())):
        event_cache = cache["events"].get(slug, {})
        if event_cache:
            metadata = event_cache.get("metadata")
            api_url = event_cache.get("api_url")
            metadata_error = event_cache.get("error")
        elif fetch and row["api_tail"]:
            api_url = metadata_endpoint(row["api_tail"])
            try:
                metadata = fetch_event(api_url)
                metadata_error = None
                event_cache = {"api_url": api_url, "metadata": metadata}
                cache["events"][slug] = event_cache
            except (HTTPError, URLError, TimeoutError, ValueError, json.JSONDecodeError) as exc:
                metadata = None
                metadata_error = f"{type(exc).__name__}: {exc}"
                event_cache = {"api_url": api_url, "error": metadata_error}
                cache["events"][slug] = event_cache
            # Lichess asks API clients to make one request at a time and reduce frequency on 429.
            time.sleep(1)
        else:
            api_url = metadata_endpoint(row["api_tail"]) if row["api_tail"] else None
            metadata = None
            metadata_error = None

        tour = metadata.get("tour") if isinstance(metadata, dict) else None
        names = sorted(row["event_names"])
        info = tour.get("info", {}) if isinstance(tour, dict) else {}
        location = info.get("location") if isinstance(info, dict) else None
        external = None
        linked_pages = []
        event_sites = sorted(row["sites"])
        broadcast_urls = sorted(row["broadcast_urls"])
        if isinstance(tour, dict) and location and not is_online_metadata(
            slug, names, tour, event_sites, broadcast_urls
        ):
            urls = [info.get("website"), info.get("standings")]
            for url in (value for value in urls if isinstance(value, str) and value):
                if url not in cache["sites"] and fetch:
                    cache["sites"][url] = linked_page_evidence(
                        url, names, str(tour.get("name") or ""), str(location)
                    )
                page = cache["sites"].get(url)
                if page:
                    linked_pages.append(page)
                    if page.get("verified") and external is None:
                        external = page
        classification, evidence = classify_event(
            slug, names, tour, external, event_sites, broadcast_urls
        )
        events.append({
            "tournament_slug": row["tournament_slug"],
            "event_identity": slug,
            "game_count": row["game_count"],
            "event_names": names,
            "sites": sorted(row["sites"]),
            "broadcast_url_examples": sorted(row["broadcast_urls"])[:3],
            "metadata_url": api_url,
            "metadata_sha256": canonical_json_hash(metadata) if metadata else None,
            "metadata": metadata,
            "metadata_error": metadata_error,
            "external_event_evidence": external,
            "linked_event_evidence": linked_pages,
            "classification": classification,
            "evidence": evidence,
        })
        if index % 5 == 0 and fetch:
            cache_path.parent.mkdir(parents=True, exist_ok=True)
            cache_path.write_text(json.dumps(cache, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
        if index % 25 == 0:
            print(f"audited {index + 1}/{len(grouped)} events", file=sys.stderr, flush=True)

    totals = {name: {"games": 0, "events": 0} for name in
              ("verified_otb", "verified_online", "unresolved")}
    for event in events:
        item = totals[event["classification"]]
        item["games"] += event["game_count"]
        item["events"] += 1
    cache_path.parent.mkdir(parents=True, exist_ok=True)
    cache_path.write_text(json.dumps(cache, ensure_ascii=False, indent=2, sort_keys=True) + "\n")

    audit = {
        "schema_version": 1,
        "scope": "Accepted games in the input Masters OTB release, grouped at Lichess broadcast tournament level",
        "input": {
            "export_name": pgn_path.name,
            "source_dataset_version": source_version,
            "sha256": hashlib.sha256(pgn_path.read_bytes()).hexdigest(),
            "games": sum(item["game_count"] for item in events),
        },
        "method": {
            "event_identity": "first path component after /broadcast/ in PGN BroadcastURL; records without a BroadcastURL are grouped by Event and Site and remain unresolved",
            "verified_online": "official tournament metadata or repeated PGN Event, Site, and BroadcastURL fields contain an explicit online, internet, or computer-engine marker; engine tournament identifiers are also recorded",
            "verified_otb": "official broadcast metadata reports a physical location and a linked event or standings page independently contains both tournament identity terms and location terms",
            "unresolved": "metadata is unavailable, or event and venue cannot be independently corroborated from linked event sources",
            "metadata_source": "Lichess Broadcast API tournament metadata plus source PGN event, site, and broadcast URL tags",
            "external_sources": "event website or standings URL linked from official tournament metadata; response content hashes and matched terms are recorded",
            "metadata_cache": cache_path.name,
        },
        "totals": totals,
        "events": events,
        "release_policy": {
            "require_verified_otb_event": True,
            "verified_online_tournament_slugs": sorted(
                event["tournament_slug"] for event in events
                if event["classification"] == "verified_online" and event["tournament_slug"]
            ),
            "verified_otb_tournament_slugs": sorted(
                event["tournament_slug"] for event in events
                if event["classification"] == "verified_otb" and event["tournament_slug"]
            ),
        },
    }
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(audit, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    return audit


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pgn", required=True, type=Path, help="PGN exported from a Masters dataset")
    parser.add_argument("--output", required=True, type=Path, help="JSON event audit output")
    parser.add_argument("--cache", required=True, type=Path, help="JSON cache for official event metadata and linked pages")
    parser.add_argument("--source-version", help="immutable version ID for the exported source dataset")
    parser.add_argument("--fetch", action="store_true", help="fetch missing Lichess and linked event metadata")
    args = parser.parse_args()
    audit = run_audit(args.pgn, args.output, args.cache, fetch=args.fetch, source_version=args.source_version)
    for classification, aggregate in audit["totals"].items():
        print(f"{classification}: {aggregate['games']} games across {aggregate['events']} events")
    print(f"Audit written to {args.output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"Masters event audit failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
