# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Complete G11 observation checker controls; fixtures below are mocks, not captures."""
from copy import deepcopy
from types import SimpleNamespace
import unittest

from agent_conformance.part_b.host_tool_binding import grade_host_route
from agent_conformance.results import CheckFailed, check


def positive():
    return {'host': 'r3', 'registration': {'host': 'r3', 'actor': 'host:r3', 'generation': 1},
        'private_token_present': True, 'token_distinct': True, 'hello': {'actor': 'host:r3'},
        'sdk_env': {'VQRO_SOCKET': '/private/s', 'VQRO_SOCKET_SOURCE': '', 'VQRO_PRINCIPAL': 'undeclared'},
        'resources': [{'id': rid, 'host': 'r3', 'backend_ref': ref, 'kind': 'agent', 'data': {}}
                      for rid, ref in (('r4', 'endpoint-a'), ('r5', 'endpoint-b'))],
        'cli': [{'key': 'doc/g11/' + ref, 'exit': 0, 'reply': {'txn': txn}} for ref, txn in (('endpoint-a', 10), ('endpoint-b', 11))],
        'history': {'txns': [{'id': txn, 'actor': 'host:r3', 'principal': 'host:r3', 'author': author,
                    'changes': [{'key': 'doc/g11/' + ref}]} for txn, author, ref in ((10, 'client:one', 'endpoint-a'), (11, 'client:two', 'endpoint-b'))]},
        'ordinary': {'actor': 'undeclared'}, 'wrong_token': 'forbidden', 'raw_host': 'invalid', 'no_grant': 'forbidden',
        'human_spawn': {'context_present': False, 'principal': 'undeclared'}, 'public_secrets_absent': True,
        'stale': {'bound': 'stale_generation', 'hello': 'forbidden', 'cli_exit': 1, 'entries': [None, None]}}


class Context:
    def __init__(self): self.evidence = []; self.backend = SimpleNamespace(env={'VQRO_SOCKET': '/private/s'})
    def check(self, value, message, **_): check(value, message, component='coordination')


class HostToolChecks(unittest.TestCase):
    def test_complete_shared_host_positive_does_not_claim_endpoint_env(self):
        ctx = Context(); grade_host_route(ctx, positive())
        self.assertEqual(len(ctx.evidence), 2)
        self.assertTrue(all(row['status'] == 'not-applicable' for row in ctx.evidence))

    def test_counterexamples_never_satisfy_complete_checker(self):
        cases = [(['host'], 'host:r3'), (['registration', 'actor'], 'owner'),
            (['registration', 'host'], 'r999'), (['registration', 'generation'], True),
            (['private_token_present'], False), (['token_distinct'], False), (['hello', 'actor'], 'endpoint-a'),
            (['sdk_env', 'VQRO_PRINCIPAL'], 'owner'), (['sdk_env', 'VQRO_SOCKET'], '/live/s'),
            (['sdk_env', 'VQRO_SOCKET_SOURCE'], 'terminal'), (['resources'], []),
            (['resources', 1, 'id'], 'r4'), (['resources', 1, 'host'], 'r8'),
            (['resources', 1, 'backend_ref'], 'endpoint-a'),
            (['resources', 1, 'data'], {'agent': {'caps': {'agent': {'tool_principal': 'env'}}}}),
            (['cli', 1, 'exit'], 1), (['cli', 1, 'exit'], False), (['cli', 1, 'reply'], {}),
            (['cli', 1, 'key'], 'doc/g11/unrelated'), (['history', 'txns'], []),
            (['history', 'txns', 1, 'principal'], 'endpoint-b'), (['history', 'txns', 1, 'actor'], 'owner'),
            (['history', 'txns', 1, 'author'], 'client:one'), (['history', 'txns', 1, 'changes'], [{'key': 'doc/unrelated'}]),
            (['ordinary', 'actor'], 'host:r3'), (['wrong_token'], None), (['raw_host'], None), (['no_grant'], None),
            (['human_spawn', 'context_present'], True), (['human_spawn', 'principal'], 'owner'),
            (['public_secrets_absent'], False), (['stale', 'bound'], None), (['stale', 'hello'], None),
            (['stale', 'cli_exit'], 0), (['stale', 'entries'], [{'value': 1}, None])]
        for path, replacement in cases:
            with self.subTest(path=path, value=replacement):
                value = deepcopy(positive()); target = value
                for key in path[:-1]: target = target[key]
                target[path[-1]] = replacement
                with self.assertRaises(CheckFailed): grade_host_route(Context(), value)


if __name__ == '__main__': unittest.main()
