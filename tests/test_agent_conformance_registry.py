"""Late row imports must bind the current implementations, not stale or overwritten handlers."""
import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from agent_conformance import cases, content_origin_cases, view_causality_cases
from agent_conformance.results import HarnessGap


class CurrentHandlerRegistryTests(unittest.TestCase):
    def test_c12_uses_current_dispatch_metadata_handler(self):
        self.assertIs(cases.HANDLERS['C12'], view_causality_cases.generic_causality_wire)

    def test_d17_uses_current_model_assertion_handler(self):
        self.assertIs(cases.HANDLERS['D17'], content_origin_cases.core_bound_origin)


class ThinDelegationTests(unittest.IsolatedAsyncioTestCase):
    async def test_o10_delegates_public_probe(self):
        ctx = object()
        with patch('agent_conformance.retention_gap_cases.public_retention_bounds', new_callable=AsyncMock) as probe:
            await cases.public_retention_bounds(ctx)
            probe.assert_awaited_once_with(ctx)

    async def test_l3_many_server_keeps_inherited_fence_after_replacement(self):
        ctx = SimpleNamespace(facts=AsyncMock(return_value={
            'host': {'owns_lifecycle': True},
            'caps': {'host': {'kind': 'server', 'endpoints': 'many'}},
        }))
        order = []
        with patch('agent_conformance.codex_l3_cases.many_host_replacement',
                   new=AsyncMock(side_effect=lambda c: order.append('replacement'))) as replacement, \
                patch.object(cases, 'host_generation_fence', side_effect=lambda c: order.append('fence')) as fence:
            await cases.host_lifecycle(ctx)
            replacement.assert_awaited_once_with(ctx)
            fence.assert_called_once_with(ctx)
        self.assertEqual(order, ['replacement', 'fence'])

    async def test_l3_replacement_failure_does_not_claim_fence_success(self):
        ctx = SimpleNamespace(facts=AsyncMock(return_value={
            'host': {'owns_lifecycle': True},
            'caps': {'host': {'kind': 'server', 'endpoints': 'many'}},
        }))
        with patch('agent_conformance.codex_l3_cases.many_host_replacement',
                   new=AsyncMock(side_effect=HarnessGap('missing host witness'))), \
                patch.object(cases, 'host_generation_fence') as fence:
            with self.assertRaises(HarnessGap):
                await cases.host_lifecycle(ctx)
            fence.assert_not_called()


if __name__ == '__main__':
    unittest.main()
