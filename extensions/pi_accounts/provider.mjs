// Public native-provider alias. No credential/file/network I/O here.
// Native Models resolves and refreshes auth under the alias provider ID.
const IDENTIFIER = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;

export function aliasProvider(providerOrGetter, id, label) {
  const resolve = typeof providerOrGetter === 'function' ? providerOrGetter : () => providerOrGetter;
  const first = resolve();
  if (!first || !IDENTIFIER.test(id) || id === first.id || typeof label !== 'string' || !label.trim()) {
    throw new Error('Invalid native provider alias');
  }
  const sourceId = first.id;
  const base = () => {
    const current = resolve();
    if (!current || current.id !== sourceId || !current.auth?.oauth) {
      throw new Error('Alias requires a native subscription OAuth provider');
    }
    if (current.refreshModels && current.refreshModelsAuth !== 'configured') {
      throw new Error('Credential-dependent native catalog aliasing is not proved');
    }
    return current;
  };
  base();
  const toAlias = model => ({...model, provider: id});
  const toBase = model => {
    if (!model || model.provider !== id) throw new Error('Model does not belong to this native alias');
    return {...model, provider: sourceId};
  };
  const route = model => {
    if (model?.api === 'pi-virtual') throw new Error('account switching unavailable for this model');
    return toBase(model);
  };
  const provider = {
    id, name: label,
    get baseUrl() {return base().baseUrl;},
    get headers() {return base().headers;},
    // Exact native OAuth functions; never silently use an ambient API-billed key.
    get auth() {return {oauth: base().auth.oauth};},
    // COMPLETE catalogs, not only available/physical models. The public getter
    // preserves later native extension overrides as well as initial composition.
    getModels: () => base().getModels().map(toAlias),
    getAllModels: () => (base().getAllModels?.() ?? base().getModels()).map(toAlias),
    stream: (model, context, options) => base().stream(route(model), context, options),
    streamSimple: (model, context, options) => base().streamSimple(route(model), context, options),
    get refreshModelsAuth() {return base().refreshModelsAuth;},
    get refreshModels() {
      return typeof base().refreshModels === 'function' ? context => base().refreshModels(context) : undefined;
    },
  };
  for (const method of ['fetchDeferred', 'cancelDeferred', 'generateImages', 'classify']) {
    Object.defineProperty(provider, method, {enumerable: true, get() {
      return typeof base()[method] === 'function' ? (model, ...args) => base()[method](route(model), ...args) : undefined;
    }});
  }
  for (const method of ['filterModels', 'filterAllModels']) {
    provider[method] = (models, ...nativeArguments) => {
      const current = base();
      const available = typeof current[method] === 'function'
        ? current[method](models.map(toBase), ...nativeArguments).map(toAlias) : models;
      // Virtual route closures remain owned by the ORIGINAL native provider.
      // Mark only alias availability unsupported; do not remove catalog rows.
      return available.filter(model => model.api !== 'pi-virtual');
    };
  }
  return provider;
}
