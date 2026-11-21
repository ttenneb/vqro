"""Pure metadata owner recovery uses existing recovery space, never archive escrow."""
import unittest
import test_coordination_engine as fixtures
import test_coordination_fence_reset as reset_fixtures
from coordination_engine import digest
from coordination_policy import PolicyError


class FenceRecoveryFunding(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.EngineTests(); self.f.setUp()
        self.f.engine.notice_fence_capacity = 2

    def pressure(self, count, size):
        for i in range(count):
            self.f.store.apply([{'op': 'set', 'key': 'pressure/' + str(i), 'expect': 0,
                                 'value': {'padding': 'x' * size}}], control=True)
        # Represent already-retained recovery-eligible metadata, not an input
        # admitted past its ceiling. Every synthetic row fits the normal row bound.
        return self.f.engine._reservation(self.f.store.view())[0]

    def args(self):
        return {'op': self.f.op(), 'action': 'reset_fences',
                'expect': self.f.engine.notice_fence_status()['revision']}

    def test_normal_share_spent_reset_fits_but_uncertain_native_control_does_not(self):
        charge = self.pressure(20, 190000)
        normal = self.f.store.byte_limit - (self.f.store.byte_limit + 9) // 10
        self.assertGreater(charge, normal)
        self.assertLess(charge + 4096, self.f.store.byte_limit - 65536)
        with self.assertRaises(PolicyError) as native:
            self.f.engine.control({'op': self.f.op(), 'target': 'r1', 'action': 'interrupt', 'args': {'v': 0}}, fixtures.OWNER)
        self.assertEqual(native.exception.kind, 'quota')
        self.assertEqual(self.f.ledger, [])
        result = self.f.engine.reset_notice_fences(self.args(), fixtures.OWNER)
        self.assertEqual(result['state'], 'recorded')
        self.assertEqual(self.f.engine.notice_fence_status()['generation'], 1)
        self.assertEqual(self.f.ledger, [])

    def test_last_archive_escrow_cannot_fund_reset(self):
        charge = self.pressure(21, 194000)
        self.assertGreater(charge, self.f.store.byte_limit - 65536)
        self.assertLess(charge + 4096, self.f.store.byte_limit)
        with self.assertRaises(PolicyError) as refusal:
            self.f.engine.reset_notice_fences(self.args(), fixtures.OWNER)
        self.assertEqual(refusal.exception.kind, 'quota')
        self.assertNotIn('notice_fences', self.f.store.get('meta')['value'])
        self.assertEqual(self.f.ledger, [])

    def test_over_normal_shrink_progresses_in_existing_bounded_pages(self):
        reset_fixtures.seed(self.f.engine, 4096, [(i * 64, digest(i), i, i) for i in range(64)])
        self.f.engine.notice_fence_capacity = 1
        charge = self.pressure(20, 190000)
        self.assertGreater(charge, self.f.store.byte_limit - (self.f.store.byte_limit + 9) // 10)
        self.f.engine.reset_notice_fences(self.args(), fixtures.OWNER)
        self.assertEqual(self.f.engine.notice_fence_status()['entries'], 40)
        self.f.engine._safe_maintain(); self.f.engine._safe_maintain()
        self.assertEqual(self.f.engine.notice_fences()[0], {digest(63): 63})
        self.assertEqual(self.f.ledger, [])


if __name__ == '__main__':
    unittest.main()
