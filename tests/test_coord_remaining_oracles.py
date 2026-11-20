# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Observed inventory and post-roll refill controls, never real runtime qualification."""
from types import SimpleNamespace
import ast
import inspect
import unittest
from unittest.mock import AsyncMock, patch

from agent_conformance.part_b.lifecycle_cases import observed_scope_inventory
from agent_conformance.part_b.scale_cases import refill_after_window_roll, full_durable_budget
from agent_conformance.results import CheckFailed
from agent_conformance.wire import RpcError


class RemainingOracles(unittest.IsolatedAsyncioTestCase):
    async def test_inventory_observes_all_ids_before_mutating_without_a_sleep(self):
        calls = []
        missing = True
        async def command(name, args):
            nonlocal missing
            self.assertEqual(name, 'scope')
            self.assertEqual(args['action'], 'inspect')
            agent = args['scope'].split(':', 1)[1]
            calls.append(agent)
            if agent == 'r1' and missing:
                missing = False
                raise RpcError({'message': 'unknown agent', 'data': {'kind': 'not_found'}})
            return {'targets': [agent], 'expect': {'membership_revision': 'stable-cut'}}
        async def wait(predicate, *, label, timeout):
            self.assertEqual(timeout, 60)
            self.assertFalse(await predicate())
            self.assertTrue(await predicate())
        ctx = SimpleNamespace(command=command, wait=wait, evidence=[])
        await observed_scope_inventory(ctx, ['r1', 'r2'])
        self.assertEqual(calls.count('r2'), 1)
        self.assertEqual(ctx.evidence[-1]['seen'], ['r1', 'r2'])

    async def test_inventory_never_certifies_a_missing_id_or_hides_other_errors(self):
        async def wait(predicate, **kwargs):
            if not await predicate():
                raise CheckFailed('missing inventory')
        ctx = SimpleNamespace(command=AsyncMock(return_value={'targets': []}), wait=wait, evidence=[])
        with self.assertRaises(CheckFailed):
            await observed_scope_inventory(ctx, ['r1'])
        self.assertEqual(ctx.evidence[-1]['kind'], 'scope_inventory_incomplete')
        ctx.command = AsyncMock(side_effect=RpcError({'message': 'unrelated conflict', 'data': {'kind': 'conflict'}}))
        with self.assertRaises(RpcError):
            await observed_scope_inventory(ctx, ['r1'])
        self.assertTrue(all(row['kind'] == 'scope_inventory_incomplete' for row in ctx.evidence))

    async def test_inventory_does_not_certify_a_changing_opaque_cut(self):
        cut = 0
        async def command(name, args):
            nonlocal cut
            cut += 1
            return {'targets': ['r1'], 'expect': {'membership_revision': str(cut)}}
        async def wait(predicate, **kwargs):
            self.assertFalse(await predicate())
            raise CheckFailed('never stable')
        ctx = SimpleNamespace(command=command, wait=wait, evidence=[])
        with self.assertRaises(CheckFailed):
            await observed_scope_inventory(ctx, ['r1'])
        self.assertEqual(ctx.evidence[-1]['kind'], 'scope_inventory_incomplete')

    async def test_late_native_deadline_starts_only_after_archive_free(self):
        source = inspect.getsource(full_durable_budget)
        tree = ast.parse(source)
        resumes, frees = [], []
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call) or not isinstance(node.func, ast.Attribute):
                continue
            if not isinstance(node.func.value, ast.Name) or node.func.value.id != 'sub':
                continue
            if node.func.attr == 'scope' and len(node.args) > 1 and isinstance(node.args[0], ast.Constant):
                if node.args[0].value == 'resume' and 'late_agent' in ast.unparse(node.args[1]):
                    resumes.append(node.lineno)
            if node.func.attr == 'command' and len(node.args) > 1 and isinstance(node.args[1], ast.Dict):
                args = {key.value: value.value for key, value in zip(node.args[1].keys, node.args[1].values)
                        if isinstance(key, ast.Constant) and isinstance(value, ast.Constant)}
                if args.get('action') == 'free':
                    frees.append(node.lineno)
        self.assertEqual(len(resumes), 1)
        self.assertEqual(len(frees), 1)
        self.assertGreater(resumes[0], frees[0])
        self.assertIn('"budget_causation": "not_asserted"', source)
        self.assertIn('a late native reply to an admitted record records its outcome at the budget', source)

    def context(self):
        def check(condition, message, **fields):
            if not condition:
                raise CheckFailed(message)
        return SimpleNamespace(evidence=[], check=check)

    async def test_refill_precedes_probe_and_uses_new_principals(self):
        order = []
        async def refill(*args, **kwargs):
            self.assertEqual(kwargs, {'prefix': 'sat-after-roll'})
            order.append('refill')
            return {'r1': ['new-held']}, object(), {'kind': 'quota'}
        async def send(**kwargs):
            order.append('probe')
            raise RpcError({'message': 'normal budget', 'data': {'kind': 'quota'}})
        ctx = self.context()
        sub = SimpleNamespace(send=send, agents=[{'id': 'r0'}, {'id': 'r1'}])
        with patch('agent_conformance.part_b.scale_cases.saturate_domain', side_effect=refill):
            result = await refill_after_window_roll(ctx, sub, [{'id': 'r1'}], object())
        self.assertEqual(order, ['refill', 'probe'])
        self.assertEqual(result['kind'], 'quota')
        self.assertEqual(ctx.evidence[-1]['kind'], 'post_roll_budget_refilled')

    async def test_refill_never_masks_an_admitted_post_roll_probe(self):
        ctx = self.context()
        sub = SimpleNamespace(send=AsyncMock(return_value={'delivery': 'admitted'}), agents=[{'id': 'r0'}, {'id': 'r1'}])
        with patch('agent_conformance.part_b.scale_cases.saturate_domain',
                   new=AsyncMock(return_value=({'r1': []}, object(), {'kind': 'quota'}))):
            with self.assertRaises(CheckFailed):
                await refill_after_window_roll(ctx, sub, [{'id': 'r1'}], object())
        self.assertEqual(sub.send.await_count, 1)


if __name__ == '__main__':
    unittest.main()
