# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""I21 workload legality and reserve-refusal inversions, not product qualification."""
import json
import unittest
from agent_conformance.part_b.priority_cases import reserve_pressure_plan, reserve_refusal
from agent_conformance.results import HarnessGap
from agent_conformance.wire import RpcError


class PriorityPressure(unittest.TestCase):
    def limits(self):
        # Public coordination-v0 quota contract; production declares its actual values.
        return {'control_reserve_percent': 10, 'quotas': {
            'content_bytes': 32768, 'domain_bytes': 4194304, 'metadata_bytes': 2048,
            'outcome_bytes': 1024, 'sender_target_bytes': 131072,
            'sender_bytes': 524288, 'target_bytes': 524288}}

    def test_encoded_content_fits_and_population_can_cross_the_unchanged_budget(self):
        limits = self.limits()
        plan = reserve_pressure_plan(limits)
        encoded = len(json.dumps([{'type': 'text', 'text': plan['text']}], separators=(',', ':')).encode())
        self.assertEqual(encoded, limits['quotas']['content_bytes'])
        self.assertEqual(encoded, plan['encoded_content_bytes'])
        self.assertGreater(plan['targets'] * encoded, plan['normal_bytes'])
        self.assertGreater(plan['targets'], 2)
        self.assertEqual(limits['quotas']['domain_bytes'], 4194304)

    def test_sender_quota_also_bounds_each_of_two_inputs(self):
        limits = self.limits()
        limits['quotas']['sender_bytes'] = 68000
        plan = reserve_pressure_plan(limits)
        self.assertLessEqual(2 * (plan['encoded_content_bytes'] + 3072), 68000)

    def test_missing_declarations_or_unreachable_fixture_are_explicit_gaps(self):
        for limits in ({}, {**self.limits(), 'control_reserve_percent': None}):
            with self.assertRaises(HarnessGap):
                reserve_pressure_plan(limits)
        limits = self.limits()
        limits['quotas']['domain_bytes'] *= 16
        with self.assertRaises(HarnessGap):
            reserve_pressure_plan(limits)

    def test_content_sender_and_recovery_quotas_are_not_control_reserve_proof(self):
        for message in ('content exceeds byte limit', 'target open-message quota exceeded',
                        'protected domain full; recovery reserve retained'):
            self.assertFalse(reserve_refusal(RpcError({'message': message, 'data': {'kind': 'quota'}})))
        self.assertTrue(reserve_refusal(RpcError({'message': 'normal byte budget would consume control reserve',
                                                'data': {'kind': 'quota'}})))
        self.assertFalse(reserve_refusal(RpcError({'message': 'normal byte budget would consume control reserve',
                                                 'data': {'kind': 'forbidden'}})))


if __name__ == '__main__':
    unittest.main()
