#!/usr/bin/env node
/** Focused real Pi1 SDK integration, no network/model API calls.
 * Usage (env-i, private HOME): node pi1-sdk.mjs SDK_PACKAGE_DIR PRIVATE_ROOT
 * CLI identity/mailbox writes are explicit doubles, not native admission evidence.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath, pathToFileURL } from 'node:url';

const [sdkPath, root] = process.argv.slice(2).map(value => path.resolve(value));
assert(sdkPath && root, 'installed SDK directory and private root required');
assert.equal(path.resolve(process.env.HOME), path.join(root, 'home'), 'private HOME required');
assert(!Object.keys(process.env).some(key => key.startsWith('HERDR_')), 'inherited Herdr environment forbidden');
assert(!process.env.VQRO_SOCKET_PATH, 'inherited Vqro socket forbidden');
fs.mkdirSync(root, { recursive: true });
for (const name of ['home', 'agent', 'work']) fs.mkdirSync(path.join(root, name), { recursive: true });
const here = path.dirname(fileURLToPath(import.meta.url));
const fixture = path.join(root, 'vqro-fixture');
fs.copyFileSync(path.join(here, 'pi1-sdk-fixture.mjs'), fixture);
fs.chmodSync(fixture, 0o700);
Object.assign(process.env, {
  VQRO_ENV: '1', VQRO_BIN_PATH: fixture, VQRO_PANE_ID: 'w1:p1',
  VQRO_SOCKET_PATH: path.join(root, 'no-native-server.sock'),
  VQRO_PI1_FIXTURE_ROOT: root, PI_CODING_AGENT_DIR: path.join(root, 'agent'),
});
// This guard makes accidental real provider/catalog requests fail the fixture.
globalThis.fetch = async () => { throw new Error('network forbidden in offline SDK fixture'); };
const sdk = await import(pathToFileURL(path.join(sdkPath, 'dist/index.js')));
const ai = await import(pathToFileURL(path.join(sdkPath, '../pi-ai/dist/index.js')));
const pkg = JSON.parse(fs.readFileSync(path.join(sdkPath, 'package.json'), 'utf8'));
assert.equal(pkg.version, '1.5.0', 'evidence is specifically installed Pi1.5.0');
const sourceFiles = [path.join(here, 'index.ts'), path.join(here, '../tasks/pi.ts'), fileURLToPath(import.meta.url), path.join(here, 'pi1-sdk-fixture.mjs')];
const hash = file => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const hashes = Object.fromEntries(sourceFiles.map(file => [path.relative(path.join(here, '../..'), file), hash(file)]));
const sdkHashes = Object.fromEntries(['package.json', 'dist/index.js', 'dist/core/sdk.js', 'dist/core/agent-session.js', 'dist/core/extensions/loader.js', 'dist/core/extensions/runner.js'].map(file => [file, hash(path.join(sdkPath, file))]));
const statePath = path.join(root, 'fixture.json');
const state = { execution: 'offline-execution', activation: false, messages: [] };
const saveState = () => fs.writeFileSync(statePath, JSON.stringify(state));
const sender = JSON.stringify({ class: 'agent', terminal_id: 'sender-terminal', agent: 'pi', execution: 'sender-execution' });
const message = id => ({ id, sender, body: `offline body ${id}` });
saveState();
const settingsManager = sdk.SettingsManager.inMemory({ compaction: { enabled: false }, retry: { enabled: false }, cacheWarm: { enabled: false } });
const modelRuntime = await sdk.ModelRuntime.create({ authPath: path.join(root, 'agent/auth.json'), modelsPath: null, modelsStorePath: path.join(root, 'agent/models-store.json'), refreshOnCreate: false, allowModelNetwork: false });
const facts = [], contexts = [], errors = [], boundaries = [];
let adapter, holdNext = false, releaseHeld, announceHeld;
const runtime = modelRuntime;
runtime.registerProvider('vqro-offline', {
  api: 'openai-completions', baseUrl: 'http://offline.invalid', apiKey: 'offline-sentinel-not-a-credential',
  models: [{ id: 'fixture', name: 'Offline deterministic fixture', reasoning: false, input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 32768, maxTokens: 1024 }],
  streamSimple(model, context) {
    contexts.push(structuredClone(context));
    const stream = ai.createAssistantMessageEventStream();
    const response = { role: 'assistant', content: [{ type: 'text', text: `offline answer ${contexts.length}` }], api: model.api, provider: model.provider, model: model.id,
      usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, stopReason: 'stop', timestamp: Date.now() };
    const finish = () => { stream.push({ type: 'done', reason: 'stop', message: response }); stream.end(response); };
    if (holdNext) {
      holdNext = false;
      releaseHeld = finish;
      announceHeld?.();
    } else queueMicrotask(finish);
    return stream;
  },
});
const loader = new sdk.DefaultResourceLoader({
  cwd: path.join(root, 'work'), agentDir: path.join(root, 'agent'), settingsManager,
  additionalExtensionPaths: sourceFiles.slice(0, 2), noSkills: true, noPromptTemplates: true, noThemes: true,
  extensionFactories: [pi => {
    pi.events.on('vqro:pi-adapter:v1:ready', value => {
      adapter = value;
      value.onFact(fact => { facts.push(fact); });
    });
    pi.events.emit('vqro:pi-adapter:v1:discover', {});
    pi.on('turn_end', event => { boundaries.push({ messageEntryId: event.messageEntryId, outcome: event.outcome }); });
  }],
});
await loader.reload();
assert.deepEqual(loader.getExtensions().errors, [], 'real extension loader must accept both source files');
const manager = sdk.SessionManager.create(path.join(root, 'work'), path.join(root, 'agent/sessions'));
const { session } = await sdk.createAgentSession({ cwd: path.join(root, 'work'), agentDir: path.join(root, 'agent'), modelRuntime, model: runtime.getModel('vqro-offline', 'fixture'), resourceLoader: loader, sessionManager: manager, settingsManager, tools: [], thinkingLevel: 'off' });
let evidence;
try {
  await session.bindExtensions({ onError: error => errors.push(error) });
  assert(adapter, 'actual SDK event-bus discovery');
  assert(facts.some(fact => fact.kind === 'session_started' && fact.self?.execution === state.execution));
  // Actual before_agent_start calls the Tasks consumer and injects its awaited claim.
  state.messages = [message('boundary-message')]; saveState();
  await session.prompt('human-started offline turn');
  assert(JSON.stringify(contexts[0]).includes('offline body boundary-message'), 'claimed context reaches the actual runtime transcript');
  assert(facts.some(fact => fact.kind === 'turn_ended' && fact.final === 'offline answer 1'));
  assert(facts.some(fact => fact.kind === 'run_settled' && fact.state === 'idle'));
  assert(!fs.existsSync(path.join(root, 'replies.jsonl')), 'boundary claim does not own an automatic reply');
  // The real Tasks idle clock explicitly activates input through the real SDK.
  state.messages = [message('idle-message')]; state.activation = true; saveState();
  await new Promise((resolve, reject) => {
    const deadline = setTimeout(() => { off(); reject(new Error('idle activated turn did not settle')); }, 15000);
    const off = adapter.onFact(fact => {
      if (fact.kind === 'run_settled' && fact.run === 2) { clearTimeout(deadline); off(); resolve(); }
    });
  });
  await session.waitForIdle();
  // Pi marks itself idle before awaited agent_settled handlers finish. Leave
  // that dispatch stack before starting a separate human turn (not a timer wait).
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(contexts.length, 2);
  const replies = fs.readFileSync(path.join(root, 'replies.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
  assert.equal(replies.length, 1);
  assert.deepEqual(replies[0], ['message', 'send', sender, 'offline answer 2', '--reply-to', 'idle-message']);
  const entries = manager.getEntries();
  assert(entries.some(entry => entry.type === 'custom' && entry.customType === 'vqro-reply-routing' && entry.data?.kind === 'replied'), 'reply routing recorded in real persisted session');
  assert.equal(errors.length, 0, JSON.stringify(errors));
  assert(boundaries.every(boundary => typeof boundary.messageEntryId === 'string'), 'Pi1 actionable boundary metadata');
  // Real SDK ordering: a durable message starts the run, a human steering
  // question is submitted while its model response is held, then the SDK starts
  // that queued user message before its final answer. No network/model spend.
  const held = new Promise(resolve => { announceHeld = resolve; });
  holdNext = true;
  state.messages = [message('human-follow-up')]; saveState();
  await Promise.race([held, new Promise((_, reject) => setTimeout(() => reject(new Error('follow-up fixture did not start')), 15000).unref())]);
  const run = adapter.snapshot().run;
  const settled = new Promise(resolve => {
    const off = adapter.onFact(fact => {
      if (fact.kind === 'run_settled' && fact.run === run) { off(); resolve(); }
    });
  });
  await session.prompt('PRIVATE human follow-up question', { streamingBehavior: 'steer' });
  releaseHeld();
  await settled;
  await session.waitForIdle();
  await new Promise(resolve => setImmediate(resolve));
  assert(facts.some(fact => fact.kind === 'input_message_started' && fact.text === 'PRIVATE human follow-up question'), 'real SDK starts queued human input');
  assert(contexts.at(-1).messages.some(message => message.role === 'user' && JSON.stringify(message.content).includes('PRIVATE human follow-up question')), 'model actually sees the human question');
  assert.equal(fs.readFileSync(path.join(root, 'replies.jsonl'), 'utf8').trim().split('\n').length, 1, 'no automatic reply record for human answer');
  assert(manager.getEntries().some(entry => entry.type === 'message' && entry.message.role === 'assistant' && JSON.stringify(entry.message.content).includes(`offline answer ${contexts.length}`)), 'human answer remains in private session');
  assert(manager.getEntries().some(entry => entry.type === 'custom' && entry.customType === 'vqro-reply-routing' && entry.data?.reason === 'follow-up-input'));
  // A changed execution while beforeRun is awaiting must discard claimed context.
  state.messages = [message('stale-message')]; state.activation = false; saveState();
  let discarded = false;
  const offBefore = adapter.beforeRun(async () => {
    state.execution = 'replacement-execution'; state.messages = []; saveState();
    return { message: { customType: 'offline-stale', content: 'NEVER INJECT STALE', display: true }, discard: () => { discarded = true; } };
  });
  await session.prompt('replacement race');
  offBefore();
  assert(discarded);
  assert(!JSON.stringify(contexts.at(-1)).includes('NEVER INJECT STALE'));
  assert(!JSON.stringify(contexts.at(-1)).includes('offline body stale-message'));
  assert(manager.getEntries().some(entry => entry.type === 'custom' && entry.customType === 'vqro-reply-routing' && entry.data?.kind === 'messages-undelivered' && entry.data.ids.includes('stale-message')));
  const oldAdapter = adapter, oldToken = adapter.snapshot();
  // Public reload performs real shutdown and rebind; old adapter cannot accept tokens.
  await session.reload();
  assert.equal(oldToken.session === adapter.snapshot().session, false);
  assert.equal(oldAdapter.current(oldToken), false);
  assert.equal(await oldAdapter.deliver('stale callback', oldToken), false);
  assert(facts.some(fact => fact.kind === 'session_closed'));
  for (const file of sourceFiles) assert.equal(hash(file), hashes[path.relative(path.join(here, '../..'), file)], 'source changed during run');
  for (const [file, expected] of Object.entries(sdkHashes)) assert.equal(hash(path.join(sdkPath, file)), expected, 'SDK changed during run');
  evidence = {
    layer: 'real Pi1 SDK / CLI double, NOT native principal admission', version: pkg.version, sdkPath, sdkHashes, sourceHashes: hashes,
    facts, boundaries, contextCount: contexts.length, replies, sessionFile: manager.getSessionFile(), errors,
  };
} finally {
  // Embedders must deliver shutdown before dispose invalidates extension APIs.
  // The CLI's AgentSessionRuntime does this; bare AgentSession.dispose() does not.
  await session.extensionRunner.emit({ type: 'session_shutdown', reason: 'quit' });
  session.dispose();
}
assert.equal(errors.length, 0, JSON.stringify(errors));
fs.writeFileSync(path.join(root, 'evidence.json'), JSON.stringify(evidence, null, 2) + '\n');
console.log(`PASS real Pi ${pkg.version} offline SDK lifecycle/context/claim/reply/reload: ${root}`);
