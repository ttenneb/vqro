#!/usr/bin/env node
/**
 * Trusted local launch adapter for Pi in Vqro. It uses only the public `vqro`
 * CLI pane operations; Pi itself reads the explicitly selected prompt file. No profile content,
 * credentials, server socket, or session file is read by this adapter.
 *
 * This is a requested-role launch, not proof of Pi's final provider prompt.
 */
import { execFile } from 'node:child_process';
import { isAbsolute } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const PANE = /^w[0-9A-Za-z]+:p[0-9A-Za-z]+$/;
const NAME = /^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$/;
const SHELL = /^(?:sh|bash|zsh)$/;
const PAUSE_MS = 200;
const THINKING = new Set(['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']);
const MAX_TEXT = 2048;

function bounded(value, label) {
  if (!value || typeof value !== 'string' || Buffer.byteLength(value, 'utf8') > MAX_TEXT || /[\u0000-\u001f\u007f]/u.test(value)) {
    throw new Error(`${label} must be nonempty, at most ${MAX_TEXT} bytes, and contain no control characters`);
  }
  return value;
}

function absolute(value, label) {
  bounded(value, label);
  if (!isAbsolute(value)) throw new Error(`${label} must be absolute`);
  return value;
}

export function parseLaunchArgs(args, env = process.env, platform = process.platform) {
  if (platform === 'win32') throw new Error('POSIX shell launch adapter is unavailable on Windows');
  const values = new Map();
  const named = new Set(['--pane', '--name', '--role-profile', '--status-extension', '--session', '--model', '--thinking', '--vqro-bin', '--pi-bin']);
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i];
    if (!named.has(key) || values.has(key) || i + 1 >= args.length || args[i + 1].startsWith('--')) {
      throw new Error(`unknown, duplicate, or missing launch argument: ${key ?? '(none)'}`);
    }
    values.set(key, args[i + 1]);
  }
  const pane = bounded(values.get('--pane'), '--pane');
  const name = bounded(values.get('--name'), '--name');
  if (!PANE.test(pane)) throw new Error('--pane must be a Vqro pane ID');
  if (!NAME.test(name)) throw new Error('--name must be a bounded agent name');
  const roleProfile = absolute(values.get('--role-profile'), '--role-profile');
  const statusExtension = absolute(values.get('--status-extension'), '--status-extension');
  const piBinary = absolute(values.get('--pi-bin'), '--pi-bin');
  const session = values.has('--session') ? absolute(values.get('--session'), '--session') : undefined;
  const model = values.has('--model') ? bounded(values.get('--model'), '--model') : undefined;
  const thinking = values.has('--thinking') ? bounded(values.get('--thinking'), '--thinking') : undefined;
  if (thinking && !THINKING.has(thinking)) throw new Error('unsupported --thinking level');
  if (model && !/^[a-zA-Z0-9][a-zA-Z0-9._:/-]{0,127}$/.test(model)) throw new Error('unsupported --model selector');
  if (env.VQRO_ENV !== '1' || !env.VQRO_SOCKET_PATH) throw new Error('a Vqro session is required');
  // A real Herdr socket in the caller is not a route for this adapter. Refuse
  // mixed environments rather than accidentally giving Pi both identities.
  if (env.HERDR_SOCKET_PATH || env.HERDR_CLIENT_SOCKET_PATH || env.HERDR_ENV === '1') {
    throw new Error('mixed Herdr/Vqro environment is refused');
  }
  const vqroBinary = absolute(values.get('--vqro-bin') ?? env.VQRO_BIN_PATH, '--vqro-bin');
  return { pane, name, roleProfile, statusExtension, piBinary, session, model, thinking, vqroBinary };
}

export function buildPiArgs(options) {
  return [
    '--append-system-prompt', options.roleProfile,
    '--extension', options.statusExtension,
    ...(options.session ? ['--session', options.session] : []),
    ...(options.model ? ['--model', options.model] : []),
    ...(options.thinking ? ['--thinking', options.thinking] : []),
  ];
}

function response(output, label) {
  let body;
  try { body = JSON.parse(output); } catch { throw new Error(`invalid Vqro ${label} response`); }
  if (body?.error) throw new Error(`Vqro ${label} rejected: ${body.error.code ?? 'unknown'}`);
  if (!body?.result) throw new Error(`Vqro ${label} returned no result`);
  return body.result;
}

function paneResponse(output, key, paneId) {
  const value = response(output, key)?.[key];
  if (!value || value.pane_id !== paneId) throw new Error(`Vqro ${key} identity mismatch`);
  return value;
}

// pane.run sends one command line to an idle shell. Quote *every* argument:
// none of the caller's paths or model choices is interpreted as shell syntax.
export function shellQuote(value) {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** `invoke` and `pause` are injectable only for isolated tests. */
export async function launchPi(options, invoke = async (binary, args) => {
  const { stdout } = await execFileAsync(binary, args, { timeout: 20_000, maxBuffer: 64 * 1024 });
  return stdout;
}, pause = wait, timeoutMs = 15_000) {
  const pane = paneResponse(await invoke(options.vqroBinary, ['pane', 'get', options.pane]), 'pane', options.pane);
  if (!pane.terminal_id) throw new Error('Vqro pane has no terminal identity');
  if (pane.agent || pane.agent_status !== 'unknown') {
    throw new Error('target pane already has agent status; no command sent');
  }
  const info = paneResponse(await invoke(options.vqroBinary, ['pane', 'process-info', '--pane', options.pane]), 'process_info', options.pane);
  const foreground = info.foreground_processes;
  if (!Number.isSafeInteger(info.shell_pid) || !Array.isArray(foreground) || foreground.length !== 1 ||
      foreground[0].pid !== info.shell_pid || !SHELL.test(foreground[0].name)) {
    throw new Error('target pane is not at an idle supported shell; no command sent');
  }
  const command = [options.piBinary, ...buildPiArgs(options)].map(shellQuote).join(' ');
  try {
    // `vqro pane run` exits 0 with *no stdout* on success. A transport error
    // has uncertain effect, so never re-send automatically.
    const sent = await invoke(options.vqroBinary, ['pane', 'run', options.pane, command]);
    if (sent.trim()) response(sent, 'pane.run');
  } catch {
    throw new Error(`Pi launch uncertain for ${options.pane}; inspect pane before retry`);
  }

  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const current = paneResponse(await invoke(options.vqroBinary, ['pane', 'get', options.pane]), 'pane', options.pane);
    if (current.terminal_id !== pane.terminal_id) throw new Error('target pane terminal changed after launch; do not retry blindly');
    if (current.agent === 'pi' && ['idle', 'working', 'blocked'].includes(current.agent_status)) {
      return {
        pane_id: options.pane,
        terminal_id: pane.terminal_id,
        local_label: options.name,
        name_registered: false,
        role: 'requested',
        profile_source: 'pi --append-system-prompt',
        status: 'Pi activity reported via public pane API; inspect /system-prompt before claiming role loaded',
      };
    }
    await pause(PAUSE_MS);
  }
  throw new Error(`Pi launch unconfirmed for ${options.pane}; command may have started. Inspect pane before retry`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const options = parseLaunchArgs(process.argv.slice(2));
    console.log(JSON.stringify(await launchPi(options)));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
