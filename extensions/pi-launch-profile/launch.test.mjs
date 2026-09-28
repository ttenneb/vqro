import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { buildPiArgs, launchPi, parseLaunchArgs as parseNativeArgs, shellQuote } from './launch.mjs';

const env = {
  VQRO_ENV: '1',
  VQRO_SOCKET_PATH: '/private/vqro.sock',
  VQRO_BIN_PATH: '/private/bin/vqro',
};
// The POSIX adapter is tested in synthetic Linux mode even on Windows CI.
const parseLaunchArgs = (args, environment = env, platform = 'linux') => parseNativeArgs(args, environment, platform);
const flags = [
  '--pane', 'w8:p1', '--name', 'worker',
  '--role-profile', '/private/roles/owner.md',
  '--status-extension', '/private/extensions/vqro-agent-state.ts',
  '--pi-bin', '/private/bin/pi',
];
const pane = (agent, agent_status = 'unknown', terminal_id = 'term_a') => ({
  result: { pane: { pane_id: 'w8:p1', terminal_id, agent, agent_status } },
});
const processInfo = (foreground = { pid: 10, name: 'sh' }) => ({
  result: { process_info: { pane_id: 'w8:p1', shell_pid: 10, foreground_processes: [foreground] } },
});
const json = (value) => JSON.stringify(value);

describe('Vqro Pi role-launch extension', () => {
  it('uses only public pane get/process-info/run and confirms Pi state through pane get', async () => {
    const opts = parseLaunchArgs([...flags, '--session', '/private/session.jsonl', '--model', 'openai-codex/gpt-6-sol', '--thinking', 'high'], env);
    const calls = [];
    const replies = [pane(), processInfo(), '', pane('pi', 'idle')];
    const result = await launchPi(opts, async (binary, args) => {
      calls.push({ binary, args });
      const reply = replies.shift();
      return typeof reply === 'string' ? reply : json(reply);
    });
    assert.equal(calls.length, 4);
    assert.deepEqual(calls.map((call) => call.binary), Array(4).fill('/private/bin/vqro'));
    assert.deepEqual(calls[0].args, ['pane', 'get', 'w8:p1']);
    assert.deepEqual(calls[1].args, ['pane', 'process-info', '--pane', 'w8:p1']);
    assert.deepEqual(calls[2].args, [
      'pane', 'run', 'w8:p1',
      "'/private/bin/pi' '--append-system-prompt' '/private/roles/owner.md' '--extension' '/private/extensions/vqro-agent-state.ts' '--session' '/private/session.jsonl' '--model' 'openai-codex/gpt-6-sol' '--thinking' 'high'",
    ]);
    assert.deepEqual(calls[3].args, ['pane', 'get', 'w8:p1']);
    assert.deepEqual(result, {
      pane_id: 'w8:p1', terminal_id: 'term_a', local_label: 'worker', name_registered: false,
      role: 'requested', profile_source: 'pi --append-system-prompt',
      status: 'Pi activity reported via public pane API; inspect /system-prompt before claiming role loaded',
    });
  });

  it('omits model, thinking and session to preserve Pi’s own defaults', () => {
    assert.deepEqual(buildPiArgs(parseLaunchArgs(flags, env)), [
      '--append-system-prompt', '/private/roles/owner.md',
      '--extension', '/private/extensions/vqro-agent-state.ts',
    ]);
  });

  it('quotes paths as one shell word; control characters and implicit paths are rejected', () => {
    assert.equal(shellQuote("/private/role's ; $(exit 1).md"), "'/private/role'\\''s ; $(exit 1).md'");
    assert.throws(() => parseLaunchArgs([...flags, '--pane', 'w9:p2'], env), /duplicate/);
    assert.throws(() => parseLaunchArgs([...flags.slice(0, 2), '--unknown', 'x', ...flags.slice(2)], env), /unknown/);
    assert.throws(() => parseLaunchArgs(flags.map((v) => v === '/private/roles/owner.md' ? 'relative.md' : v), env), /absolute/);
    assert.throws(() => parseLaunchArgs(flags.map((v) => v === 'w8:p1' ? 'w8:p1\nherdr' : v), env), /control/);
  });

  it('refuses mixed Herdr/Vqro environments and non-POSIX shells', () => {
    assert.throws(() => parseLaunchArgs(flags, env, 'win32'), /unavailable on Windows/);
    assert.throws(() => parseLaunchArgs(flags, { ...env, HERDR_SOCKET_PATH: '/real/herdr.sock' }), /mixed Herdr/);
    assert.throws(() => parseLaunchArgs(flags, { ...env, HERDR_ENV: '1' }), /mixed Herdr/);
    assert.throws(() => parseLaunchArgs(flags, { ...env, VQRO_ENV: '0' }), /Vqro session/);
    assert.throws(() => parseLaunchArgs(flags, { ...env, VQRO_SOCKET_PATH: '' }), /Vqro session/);
  });

  it('does not send pane.run on stale agent status, changed identity, or occupied shell', async () => {
    const opts = parseLaunchArgs(flags, env);
    for (const reply of [
      { result: { pane: { pane_id: 'w9:p2', terminal_id: 'term_a' } } },
      pane('pi', 'idle'),
      pane(),
    ]) {
      let calls = 0;
      await assert.rejects(launchPi(opts, async () => {
        calls++;
        return json(calls === 1 ? reply : processInfo({ pid: 12, name: 'pi' }));
      }), /identity mismatch|already has agent|idle supported shell/);
      assert.ok(calls <= 2);
    }
  });

  it('rejects a switched pane after sending, without retrying an uncertain launch', async () => {
    const opts = parseLaunchArgs(flags, env);
    const replies = [pane(), processInfo(), '', pane('pi', 'idle', 'term_replaced')];
    await assert.rejects(launchPi(opts, async () => {
      const reply = replies.shift();
      return typeof reply === 'string' ? reply : json(reply);
    }), /do not retry blindly/);
  });

  it('reports unconfirmed launch; it never calls a second pane.run', async () => {
    const opts = parseLaunchArgs(flags, env);
    let calls = 0;
    const replies = [pane(), processInfo(), ''];
    await assert.rejects(launchPi(opts, async (_bin, args) => {
      if (args[1] === 'run') calls++;
      const reply = replies.shift();
      return typeof reply === 'string' ? reply : json(reply);
    }, async () => {}, 0), /may have started/);
    assert.equal(calls, 1);
  });
});
