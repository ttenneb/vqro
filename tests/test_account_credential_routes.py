"""Known credential aliases and visible-but-unselectable metadata; no auth I/O."""
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0] = [str(Path(__file__).resolve().parents[1] / 'extensions'), str(Path(__file__).resolve().parent)]
from agent_fixture import FakeCore
from account_credentials import SETTINGS, validate, snapshot, choose, selection_problem, credential_identity, distinct_accounts, eligible_alternatives
from vqro_ext import VqroError


def row(ident='a', runtime='claude', **extra):
    route = {'secret_ref': 'a' * 32, 'host': 'host-1'} if runtime == 'claude' else \
            {'provider': 'anthropic', 'native_provider': 'native-alias'} if runtime == 'pi' else \
            {'login_ref': {'sessionId': 'saved-login', 'accountId': 'workspace-account'}}
    return dict(id=ident, label=ident.upper(), runtime=runtime, kind='subscription', **dict(route, **extra))


class CredentialRoutes(unittest.TestCase):
    def core(self, rows):
        core = FakeCore('accounts')
        core.apply([{'op': 'set', 'key': SETTINGS, 'value': {'accounts': rows}}], 'fake registry')
        return core

    def test_same_known_native_reference_is_one_account_in_each_runtime(self):
        for runtime in ('pi', 'codex', 'claude'):
            a, b = row(runtime=runtime), row('b', runtime)
            self.assertEqual(credential_identity(a), credential_identity(b))
            self.assertEqual([item['id'] for item in distinct_accounts([a, b])], ['a'])
            self.assertEqual([item['id'] for item in snapshot(self.core([a, b]))[0]], ['a', 'b'])

    def test_native_default_provider_and_explicit_spelling_are_aliases(self):
        for runtime, provider in (('codex', 'openai'), ('claude', 'anthropic')):
            implicit = row(runtime=runtime)
            explicit = row('alias', runtime, provider=provider)
            self.assertEqual(credential_identity(implicit), credential_identity(explicit))

    def test_api_key_without_its_provider_route_is_shown_but_not_selectable(self):
        account = dict(row(), kind='api_key')
        core = self.core([account])
        self.assertEqual(snapshot(core)[0], [account])
        self.assertTrue(selection_problem(account))
        with self.assertRaises(VqroError): choose(core, {'account':'a'}, 'claude', 'fake.new')

    def test_distinct_native_namespaces_are_not_deduplicated_or_authenticated(self):
        a = row()
        self.assertNotEqual(credential_identity(a), credential_identity(row('b', host='host-2')))
        self.assertNotEqual(credential_identity(a), credential_identity(row('b', provider='other-provider')))
        api = dict(row('b'), kind='api_key', provider='anthropic')
        self.assertNotEqual(credential_identity(a), credential_identity(api))
        self.assertNotEqual(credential_identity(row(runtime='pi')), credential_identity(row(runtime='codex')))

    def test_conflicting_known_routes_are_shown_but_not_selectable(self):
        bad = [dict(row(runtime='codex'), kind='api_key', secret_ref='b' * 32, host='host-1'),
               dict(row(runtime='pi'), kind='api_key', secret_ref='b' * 32, host='host-1'),
               dict(row(runtime='claude'), login_ref={'sessionId': 's', 'accountId': 'w'})]
        for item in bad:
            with self.subTest(item=item):
                self.assertEqual(validate(item), item)
                core = self.core([item])
                self.assertEqual(snapshot(core)[0], [item])
                self.assertTrue(selection_problem(item))
                with self.assertRaises(VqroError): choose(core, {'account': 'a'}, item['runtime'], 'fake.new')
                self.assertFalse(any(name == 'resource.spawn' for name, _ in core.calls))

    def test_missing_host_or_reference_is_visible_but_not_selectable(self):
        for item in (dict(row(), host=None), {key: value for key, value in row().items() if key != 'host'},
                     {key: value for key, value in row().items() if key != 'secret_ref'},
                     {'id':'a','label':'A','runtime':'codex','kind':'subscription'},
                     {'id':'a','label':'A','runtime':'pi','kind':'subscription'}):
            if item.get('host', 'present') is None:
                continue  # Invalid field types still fail structural validation, not readiness.
            with self.subTest(item=item):
                core = self.core([item])
                self.assertEqual(snapshot(core)[0], [item])
                self.assertTrue(selection_problem(item))
                with self.assertRaises(VqroError): choose(core, {'account':'a'}, item['runtime'], 'fake.new')

    def test_chooser_marks_invalid_rows_without_a_launch_action(self):
        invalid = {key: value for key, value in row().items() if key != 'host'}
        core = self.core([invalid])
        choose(core, {}, 'claude', 'fake.new')
        items = core.live['accounts-choice-fake-new']['items']
        self.assertTrue(any(item.get('style') == 'warn' and 'invalid' in item.get('text', '').lower() for item in items))
        self.assertFalse(any(item.get('command') and item.get('args', {}).get('account') == 'a' for item in items))

    def test_source_alias_and_blocked_target_alias_are_never_alternatives(self):
        a, alias = row(), row('alias')
        b, b_alias = row('b', secret_ref='b' * 32), row('b-alias', secret_ref='b' * 32)
        self.assertEqual(eligible_alternatives([a, alias, b, b_alias], 'a', {'b'}, False), [])

    def test_real_round_robin_has_only_one_slot_for_known_aliases(self):
        import accounts_policy as policy
        core = self.core([row(), row('alias')])
        core.entries[policy.POLICY] = {'version': 1, 'value': {'new_agents': 'round_robin'}}
        first, _ = choose(core, {}, 'claude', 'fake.new')
        second, _ = choose(core, {}, 'claude', 'fake.new')
        self.assertEqual((first['id'], second['id']), ('a', 'a'))

    def test_real_strategies_exclude_a_credential_when_any_alias_is_blocked(self):
        import accounts_policy as policy
        from test_account_capacity import resource, NOW
        for mode in ('round_robin', 'most_remaining_capacity'):
            with self.subTest(mode=mode):
                a, alias = row(), row('alias')
                core = self.core([a, alias])
                core.entries[policy.POLICY] = {'version': 1, 'value': {'new_agents': mode}}
                core.resources['healthy-a'] = resource('healthy-a', a, 10)
                core.resources['blocked-alias'] = resource('blocked-alias', alias, 100, blocked=True)
                with patch('accounts_policy.now_ms', return_value=NOW):
                    selected, popup = choose(core, {}, 'claude', 'fake.new')
                self.assertIsNone(selected)
                self.assertIsNotNone(popup)
                self.assertNotIn(policy.CURSORS + 'claude', core.entries)

    def test_real_most_capacity_preserves_restrictive_and_unknown_alias_reports(self):
        import accounts_policy as policy
        from test_account_capacity import resource, NOW
        for case in ('exhausted', 'missing', 'incomplete', 'stale'):
            with self.subTest(case=case):
                a, alias = row(), row('alias')
                core = self.core([a, alias])
                core.entries[policy.POLICY] = {'version': 1, 'value': {'new_agents': 'most_remaining_capacity'}}
                core.resources['healthy-a'] = resource('healthy-a', a, 10)
                report = resource('alias-report', alias, None if case == 'missing' else 100 if case == 'exhausted' else 10)
                if case == 'incomplete': report['data']['agent']['incomplete'] = ['usage_limits']
                if case == 'stale': report['data']['agent']['usage_limits'][0]['observed_at_ms'] = NOW - policy.MAX_AGE_MS - 1
                core.resources['alias-report'] = report
                with patch('accounts_policy.now_ms', return_value=NOW):
                    selected, popup = choose(core, {}, 'claude', 'fake.new')
                self.assertIsNone(selected)
                self.assertIsNotNone(popup)

    def test_fresh_id_selection_needs_no_revision_but_supplied_revision_is_strict(self):
        account = row()
        core = self.core([account])
        self.assertEqual(choose(core, {'account':'a'}, 'claude', 'fake.new')[0], account)
        revision = core.version(SETTINGS)
        for invalid in (True, float(revision), str(revision)):
            with self.subTest(invalid=invalid), self.assertRaises(VqroError):
                choose(core, {'account':'a','_account_revision':invalid}, 'claude', 'fake.new')
        self.assertEqual(choose(core, {'account':'a','_account_revision':revision}, 'claude', 'fake.new')[0], account)


if __name__ == '__main__': unittest.main()
