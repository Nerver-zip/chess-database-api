#!/usr/bin/env python3
"""Offline regression tests for event-level Masters provenance auditing."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "scripts"))
import audit_masters_events as audit


class MastersEventAuditTests(unittest.TestCase):
    def test_redirect_handler_rejects_private_destinations(self):
        handler = audit.PublicRedirectHandler()
        request = audit.Request("https://example.org/event")
        self.assertIsNone(handler.redirect_request(
            request, None, 302, "Found", {}, "http://127.0.0.1/private"
        ))
        self.assertIsNone(handler.redirect_request(
            request, None, 302, "Found", {}, "http://169.254.169.254/latest"
        ))

    def test_event_metadata_classification_uses_venue_and_online_evidence(self):
        classification, evidence = audit.classify_event(
            "tata-steel-2026",
            ["Tata Steel Masters"],
            {"name": "Tata Steel Masters", "info": {"location": "Wijk aan Zee, Netherlands"}},
            {"verified": True, "url": "https://official.example/tata-steel"},
        )
        self.assertEqual(classification, "verified_otb")
        self.assertIn("Wijk aan Zee", evidence[0])

        classification, evidence = audit.classify_event(
            "titled-tuesday",
            ["Titled Tuesday"],
            {"name": "Titled Tuesday", "info": {"format": "Online rapid"}},
        )
        self.assertEqual(classification, "verified_online")
        self.assertTrue(evidence)

        classification, evidence = audit.classify_event(
            "unknown-event", ["Open"], {"name": "Open", "info": {}}
        )
        self.assertEqual(classification, "unresolved")
        self.assertTrue(evidence)

        # A chess platform hosting an event page does not prove that the games were online.
        classification, evidence = audit.classify_event(
            "ambiguous-event",
            ["Regional Championship"],
            {"name": "Regional Championship", "info": {
                "location": "Oslo, Norway", "website": "https://www.chess.com/events",
            }},
        )
        self.assertEqual(classification, "unresolved")
        self.assertTrue(evidence)

        classification, evidence = audit.classify_event(
            "regional-championship",
            ["Regional Championship"],
            {"name": "Regional Championship", "info": {"location": "Oslo, Norway"}},
            {"verified": True, "url": "https://official.example/regional"},
            sites=["Internet Chess Club"],
        )
        self.assertEqual(classification, "verified_online")
        self.assertTrue(any("PGN Site tag" in item for item in evidence))

    def test_repeated_games_are_aggregated_by_tournament_and_policy_is_explicit(self):
        pgn = """[Event "Open"]
[Site "Lichess Broadcast"]
[BroadcastURL "https://lichess.org/broadcast/otb-tour/round-1/Round001"]

1. e4 e5 *

[Event "Open Round 2"]
[Site "Lichess Broadcast"]
[BroadcastURL "https://lichess.org/broadcast/otb-tour/round-2/Round002"]

1. d4 d5 *

[Event "Titled Tuesday"]
[Site "Lichess Broadcast"]
[BroadcastURL "https://lichess.org/broadcast/online-tour/round-1/Round003"]

1. Nf3 d5 *
"""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pgn_path = root / "accepted.pgn"
            output_path = root / "audit.json"
            cache_path = root / "metadata.json"
            pgn_path.write_text(pgn)
            cache_path.write_text(json.dumps({
                "events": {
                "otb-tour": {
                    "api_url": "https://lichess.org/api/broadcast/otb-tour/round-1/Round001",
                    "metadata": {
                        "tour": {
                            "id": "OtBTourId",
                            "name": "Open",
                            "url": "https://lichess.org/broadcast/otb-tour",
                            "info": {"location": "Prague, Czechia", "website": "https://official.example/otb-tour"},
                        }
                    },
                },
                "online-tour": {
                    "api_url": "https://lichess.org/api/broadcast/online-tour/round-1/Round003",
                    "metadata": {
                        "tour": {
                            "id": "OnlineTourId",
                            "name": "Titled Tuesday",
                            "url": "https://lichess.org/broadcast/online-tour",
                            "info": {"format": "Online blitz"},
                        }
                    },
                },
                },
                "sites": {
                    "https://official.example/otb-tour": {
                        "url": "https://official.example/otb-tour",
                        "final_url": "https://official.example/otb-tour",
                        "verified": True,
                        "matched_event_terms": ["open", "tour"],
                        "matched_location_terms": ["prague"],
                    }
                },
            }))
            result = audit.run_audit(pgn_path, output_path, cache_path, fetch=False)
            self.assertEqual(result["input"]["games"], 3)
            self.assertEqual(result["totals"]["verified_otb"], {"games": 2, "events": 1})
            self.assertEqual(result["totals"]["verified_online"], {"games": 1, "events": 1})
            otb_event = next(event for event in result["events"]
                             if event["classification"] == "verified_otb")
            self.assertEqual(len(otb_event["linked_event_evidence"]), 1)
            self.assertTrue(otb_event["linked_event_evidence"][0]["verified"])
            self.assertEqual(
                result["release_policy"]["verified_otb_tournament_slugs"], ["otb-tour"]
            )
            self.assertEqual(
                result["release_policy"]["verified_online_tournament_slugs"], ["online-tour"]
            )
            self.assertEqual(json.loads(output_path.read_text()), result)


if __name__ == "__main__":
    unittest.main()
