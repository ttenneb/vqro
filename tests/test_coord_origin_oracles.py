# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""C2 origin-envelope oracle inversions; no runtime execution or regrade."""
import unittest
from agent_conformance.part_b.cases import inbox_dispatch_origin


class InboxOriginOracle(unittest.TestCase):
    def origin(self, principal='agent-A'):
        return {'principal': 'extension:coord', 'actor': 'extension:coord', 'kind': 'tool',
                'sender': {'principal': principal, 'actor': principal, 'provenance': 'inbox-record'}}

    def test_original_senders_are_distinct_from_the_core_transport_identity(self):
        for principal in ('agent-A', 'worker:persistent', 'worker:original-reviewer'):
            self.assertTrue(inbox_dispatch_origin(self.origin(principal), principal))

    def test_correct_sender_does_not_excuse_wrong_core_principal_or_actor(self):
        for key in ('principal', 'actor'):
            for spoofed in ('owner', 'agent-A', 'extension:other'):
                origin = self.origin()
                origin[key] = spoofed
                self.assertFalse(inbox_dispatch_origin(origin, 'agent-A'))

    def test_owner_quote_cannot_replace_the_admitted_inbox_sender(self):
        model = {'sender': self.origin('owner'), 'quoted_claim': {'principal': 'agent-A', 'kind': 'agent'}}
        self.assertFalse(inbox_dispatch_origin(model['sender'], 'agent-A'))
        self.assertFalse(inbox_dispatch_origin(self.origin(), 'owner'))

    def test_original_sender_requires_the_exact_inbox_record_provenance(self):
        for provenance in (None, 'core', 'caller', ''):
            origin = self.origin()
            origin['sender']['provenance'] = provenance
            self.assertFalse(inbox_dispatch_origin(origin, 'agent-A'))
        origin = self.origin()
        del origin['sender']['provenance']
        self.assertFalse(inbox_dispatch_origin(origin, 'agent-A'))

    def test_flat_legacy_origin_is_not_a_valid_c2_dispatch_envelope(self):
        self.assertFalse(inbox_dispatch_origin({'principal': 'agent-A', 'actor': 'agent-A'}, 'agent-A'))
        origin = self.origin()
        del origin['sender']['principal']
        self.assertFalse(inbox_dispatch_origin(origin, 'agent-A'))

    def test_malformed_or_unknown_identity_never_matches(self):
        for origin in (None, [], 'invalid'):
            self.assertFalse(inbox_dispatch_origin(origin, 'agent-A'))
        for sender in (None, [], 'invalid'):
            origin = self.origin()
            origin['sender'] = sender
            self.assertFalse(inbox_dispatch_origin(origin, 'agent-A'))
        for principal in (None, '', 1):
            self.assertFalse(inbox_dispatch_origin(self.origin(), principal))


if __name__ == '__main__':
    unittest.main()
