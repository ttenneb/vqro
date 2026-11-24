// Installed as the first explicit native extension, never into a user directory.
// Only public Pi APIs. Native Pi alone opens credentials, logs in and refreshes.
import { aliasProvider } from './provider.mjs';
const ID = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function accountMetadata(raw) {
  if (typeof raw !== 'string' || Buffer.byteLength(raw, 'utf8') > 2048) throw new Error('Invalid account metadata');
  const value = JSON.parse(raw);
  if (!value || typeof value !== 'object' || Array.isArray(value)
      || Object.keys(value).sort().join(',') !== 'id,label,native_provider,provider'
      || ![value.id, value.provider, value.native_provider].every(item => typeof item === 'string' && ID.test(item))
      || value.native_provider === value.provider || typeof value.label !== 'string'
      || !value.label.trim() || [...value.label].length > 128 || /[\x00-\x1f\x7f-\x9f\uD800-\uDFFF]/u.test(value.label)) {
    throw new Error('Invalid account metadata');
  }
  return value;
}

export default function accountExtension(pi) {
  pi.registerFlag('vqro-account', {description: 'Nonsecret configured account metadata', type: 'string'});
  pi.registerFlag('vqro-account-login', {description: 'Register account for explicit native /login', type: 'boolean', default: false});
  let account;
  let ready = false;
  let loginOnly = false;
  let failed = false;
  let latestErrorTimestamp;
  // Metadata only, never history contents. New work invalidates the old error
  // before a quiescent shutdown can act on it, even within one adapter epoch.
  pi.on('agent_start', () => {latestErrorTimestamp = undefined;});
  pi.on('message_end', event => {
    const message = event.message;
    latestErrorTimestamp = message?.role === 'assistant' && message.stopReason === 'error'
      && Number.isSafeInteger(message.timestamp) && message.timestamp >= 0 ? message.timestamp : undefined;
  });

  function refuse(ctx, message) {
    failed = true;
    ready = false;
    try {ctx.ui.notify(message, 'error');} catch { /* UI failure must not disable the guard. */ }
    ctx.shutdown();
  }

  // A failing session_start handler is swallowed by native Pi. Explicitly stop,
  // and guard ALL native input sources (including extension-origin new turns)
  // while shutdown settles. No earlier input is ever saved or replayed here.
  pi.on('input', (_event, ctx) => {
    // getFlag is available when native input can run, including an earlier
    // extension's session_start. Unparsed declared metadata is not normal login.
    const declared = pi.getFlag('vqro-account') !== undefined;
    const bound = (!declared && !account) || (ready && (loginOnly || (account
      && ctx.model?.provider === account.native_provider && ctx.model?.api !== 'pi-virtual')));
    if (failed || !bound) {
      try {ctx.ui.notify('Account setup unavailable; nothing submitted. Inspect Accounts/native status.', 'error');} catch { /* Fail closed without UI. */ }
      return {action: 'handled'};
    }
    return {action: 'continue'};
  });

  pi.on('session_start', async (_event, ctx) => {
    const raw = pi.getFlag('vqro-account');
    if (raw === undefined) {ready = true; return;}
    try {
      account = accountMetadata(raw);
      loginOnly = pi.getFlag('vqro-account-login') === true;
      const source = ctx.modelRegistry.getProvider(account.provider);
      if (!source) throw new Error('Native base provider unavailable');
      if (ctx.modelRegistry.getProvider(account.native_provider)) throw new Error('Native alias provider already exists');
      const alias = aliasProvider(() => ctx.modelRegistry.getProvider(account.provider), account.native_provider, account.label);
      pi.registerProvider(alias);
      // Availability reads native credential metadata only; never allow a
      // network catalog refresh as a side effect of account registration.
      await ctx.modelRegistry.refresh({allowNetwork: false});
      if (loginOnly) {
        ready = true;
        ctx.ui.notify('Use native /login and choose ' + account.label + '. vqro does not inspect login success.', 'info');
        return;
      }
      let current = ctx.model;
      if (!current || ![account.provider, account.native_provider].includes(current.provider)) {
        // Before aliases exist, native startup may not restore an alias model.
        // Recover its MODEL METADATA via public native context/settings only;
        // never pick another model, inspect history content or read a file.
        const restored = ctx.sessionManager.buildSessionContext().model;
        const savedId = restored?.modelId ?? pi.getSettings().defaultModel;
        if (typeof savedId === 'string') {
          current = ctx.modelRegistry.getModelOfType('chat', account.provider, savedId);
        }
      }
      if (!current || ![account.provider, account.native_provider].includes(current.provider)) {
        throw new Error('Native initial model does not match the account provider');
      }
      if (current.api === 'pi-virtual') {
        refuse(ctx, 'account switching unavailable for this model');
        return;
      }
      const model = ctx.modelRegistry.getModelOfType('chat', account.native_provider, current.id);
      if (!model || !await pi.setModel(model)) throw new Error('Native account sign-in required');
      ready = true;
    } catch {
      // Do not expose raw native errors or flag values in a public notice.
      refuse(ctx, 'Native account setup unavailable; use Accounts sign-in. No turn submitted.');
    }
  });

  pi.registerCommand('vqro-account-quiesce', {
    description: 'Gracefully end an idle native process for a same-conversation account restart',
    handler: async (args, ctx) => {
      let request;
      try {
        request = JSON.parse(args);
        if (!request || Object.keys(request).sort().join(',') !== 'error_timestamp,file,model,provider,session'
            || typeof request.session !== 'string' || !UUID.test(request.session)
            || typeof request.provider !== 'string' || !ID.test(request.provider)
            || typeof request.file !== 'string' || !request.file || request.file.length > 4096 || /[\x00-\x1f\x7f]/.test(request.file)
            || typeof request.model !== 'string' || !request.model || request.model.length > 256
            || !Number.isSafeInteger(request.error_timestamp) || request.error_timestamp < 0) throw new Error();
      } catch {
        ctx.ui.notify('Invalid quiescent-restart request; nothing stopped.', 'error');
        return;
      }
      if (!ctx.isIdle() || ctx.hasPendingMessages() || latestErrorTimestamp !== request.error_timestamp
          || ctx.sessionManager.getSessionId() !== request.session || ctx.sessionManager.getSessionFile() !== request.file
          || ctx.model?.provider !== request.provider || ctx.model?.id !== request.model || ctx.model?.api === 'pi-virtual') {
        ctx.ui.notify('Native session/model changed, is unsupported or active; nothing stopped.', 'error');
        return;
      }
      // Ordinary native CLI/RPC shutdown waits ModelRuntime.settleCredentialWrites.
      // The caller MUST observe a successful old-process exit before spawning B.
      ctx.shutdown();
    },
  });
}
