"""Scan-local derived facts: bounded copies, no cross-scan authorization cache."""
import tempfile
import unittest
import test_coordination_engine as fixtures


class DispatchScan(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.EngineTests(); self.f.setUp()

    def count_records(self):
        original = self.f.engine.records
        calls = []
        def records():
            calls.append(True)
            return original()
        self.f.engine.records = records
        return calls

    def test_native_scan_copies_history_once_for_eight_candidates(self):
        for _ in range(8):
            self.f.send(mode='admit', send_new=True)
        self.f.resume()
        calls = self.count_records()
        self.assertIsNotNone(self.f.engine.claim_one())
        self.assertEqual(len(calls), 1)
        self.assertEqual(self.f.ledger, [])

    def test_pull_scan_copies_history_once_for_eight_candidates(self):
        self.f.setup_pull()
        for _ in range(8):
            self.f.send(send_new=True)
        calls = self.count_records()
        taken = self.f.engine.take({'op': self.f.op(), 'agent': 'r1', 'consumer': 'c1', 'at': 'idle', 'limit': 8}, fixtures.OWNER)
        self.assertEqual(len(taken['records']), 8)
        self.assertEqual(len(calls), 1)
        self.assertEqual(self.f.ledger, [])

    def test_unknown_attention_heads_are_rebuilt_between_scans(self):
        first = self.f.send(mode='admit')
        self.f.send(mode='admit', send_new=True)
        self.f.resume()
        self.f.engine.attention[first['delivery']] = 'outcome_persistence_failed'
        self.assertIsNone(self.f.engine.claim_one())
        self.f.engine.attention.clear()
        self.assertEqual(self.f.engine.claim_one()['delivery'], first['delivery'])

    def test_archive_abort_rebuilds_frozen_selection_without_rewriting_frozen_record(self):
        sent = self.f.send(mode='admit'); self.f.resume()
        with tempfile.TemporaryDirectory() as path:
            archive = self.f.engine.archive({'op': self.f.op(), 'agents': ['r1'],
                'deliveries': [sent['delivery']], 'destination': path}, fixtures.OWNER)['archive']
            before = self.f.store.get('message/' + sent['delivery'])
            self.assertIsNone(self.f.engine.claim_one())
            self.assertEqual(self.f.store.get('message/' + sent['delivery']), before)
            self.f.engine.archive({'op': self.f.op(), 'action': 'abort', 'archive': archive}, fixtures.OWNER)
            self.assertEqual(self.f.engine.claim_one()['delivery'], sent['delivery'])

    def test_authority_is_still_current_at_native_effect_after_claim(self):
        context = {'principal': 'p:sender', 'actor': 'p:sender', 'author': 'client:test', 'kind': 'tool'}
        key = 'doc/agents/grants/send'
        self.f.config[key] = {'version': 1, 'value': {'who': 'p:sender', 'on': 'agent:r1', 'can': ['send:admit']}}
        op = self.f.op()
        self.f.engine.send({'op': op, 'submission': op, 'to': 'r1', 'mode': 'admit',
                            'content': [{'type': 'text', 'text': 'do not send after revocation'}]}, context)
        self.f.resume()
        claimed = self.f.engine.claim_one()
        del self.f.config[key]
        result = self.f.engine.execute_claim(claimed)
        self.assertEqual(self.f.ledger, [])
        self.assertEqual(result['reason'], 'no_grant')


if __name__ == '__main__':
    unittest.main()
