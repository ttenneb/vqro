#!/usr/bin/env node
// Offline CLI double for pi1-sdk.mjs. This is NOT native principal admission.
import fs from 'node:fs';
import path from 'node:path';
const root = process.env.VQRO_PI1_FIXTURE_ROOT;
if (!root) throw new Error('private fixture root required');
const statePath = path.join(root, 'fixture.json');
const state = JSON.parse(fs.readFileSync(statePath, 'utf8'));
const args = process.argv.slice(2);
fs.appendFileSync(path.join(root, 'cli.jsonl'), JSON.stringify(args) + '\n');
let result;
if (args[0] === 'agent' && args[1] === 'get') {
  result = { result: { agent: { name: 'offline', execution: state.execution, terminal_id: 'offline-terminal', pane_id: 'w1:p1' } } };
} else if (args[0] === 'message' && args[1] === 'list') {
  result = { activation: state.activation, waiting: state.messages };
} else if (args[0] === 'message' && args[1] === 'claim') {
  result = { claimed: state.messages.map(message => ({ ...message, attempt: { id: `attempt-${message.id}`, outcome: 'pending' } })) };
  state.messages = [];
  fs.writeFileSync(statePath, JSON.stringify(state));
} else if (args[0] === 'message' && args[1] === 'outcomes') {
  fs.appendFileSync(path.join(root, 'outcomes.jsonl'), JSON.stringify(args) + '\n');
  result = { outcomes: JSON.parse(args[2]).map(item => ({ id: item.id, attempt: { id: item.attempt, outcome: args[3] } })) };
} else if (args[0] === 'task' && args[1] === 'whoami') {
  result = { caller: { class: 'agent' } };
} else if (args[0] === 'message' && args[1] === 'send') {
  fs.appendFileSync(path.join(root, 'replies.jsonl'), JSON.stringify(args) + '\n');
  result = { message: { state: 'waiting', revision: 1 } };
} else if (args[0] === 'pane' && args[1] === 'report-metadata') {
  result = { result: {} };
} else {
  throw new Error(`unexpected fixture invocation: ${JSON.stringify(args)}`);
}
console.log(JSON.stringify(result));
