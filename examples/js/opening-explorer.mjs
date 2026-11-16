// Dependency-free client for the Opening Explorer HTTP API.
//
// Requires Node.js 18 or later (global fetch, AbortSignal.timeout). The same
// file also runs in browsers when the server allows the page's origin through
// --allowed-origins.
//
// Game IDs are 64-bit integers and can exceed Number.MAX_SAFE_INTEGER. This
// client therefore keeps IDs as decimal strings and never converts them to
// Number.

export const COLLECTIONS = Object.freeze(['masters-otb', 'elite-online']);

const MAX_GAME_ID = 18446744073709551615n; // 2^64 - 1

/** Error raised for any non-2xx response or network failure. */
export class OpeningExplorerError extends Error {
  /**
   * @param {number} status HTTP status code; 0 for network or timeout failures.
   * @param {string} code Stable machine-readable code such as "invalid_moves".
   *   Branch on this value; the message text may change between releases.
   * @param {string} message Human-readable description.
   */
  constructor(status, code, message) {
    super(`HTTP ${status} ${code}: ${message}`);
    this.name = 'OpeningExplorerError';
    this.status = status;
    this.code = code;
  }
}

/**
 * Client for one Opening Explorer server.
 *
 * Position responses are cached in memory by URL with their ETag. Repeated
 * requests send If-None-Match and reuse the cached body on 304 Not Modified.
 */
export class OpeningExplorerClient {
  /**
   * @param {object} [options]
   * @param {string} [options.baseUrl='http://127.0.0.1:8080'] Server origin, without a trailing path.
   * @param {number} [options.timeoutMs=10000] Per-request timeout in milliseconds.
   * @param {typeof fetch} [options.fetchImpl=globalThis.fetch] Injectable fetch (useful for tests).
   */
  constructor({
    baseUrl = 'http://127.0.0.1:8080',
    timeoutMs = 10_000,
    fetchImpl = globalThis.fetch,
  } = {}) {
    this.baseUrl = baseUrl.replace(/\/+$/, '');
    this.timeoutMs = timeoutMs;
    this.fetchImpl = fetchImpl;
    this.etagCache = new Map();
  }

  /** GET /health: liveness and software version. */
  async health() {
    return this.#getJson('/health');
  }

  /** GET /ready: resolves when at least one collection is loaded. */
  async ready() {
    return this.#getJson('/ready');
  }

  /**
   * GET /v1/collections: metadata for each loaded collection.
   * @returns {Promise<object[]>}
   */
  async collections() {
    const body = await this.#getJson('/v1/collections');
    return body.collections;
  }

  /**
   * GET /v1/explorer/position: statistics for one position.
   * Provide exactly one of `moves` (SAN or UCI, as a string or an array; an
   * empty string or empty array means the initial position) or `fen`.
   *
   * @param {object} query
   * @param {'masters-otb'|'elite-online'} query.collection
   * @param {string|string[]} [query.moves]
   * @param {string} [query.fen]
   * @returns {Promise<object>} Position response (see docs/API_REFERENCE.md).
   */
  async position({ collection, moves, fen }) {
    if (!COLLECTIONS.includes(collection)) {
      throw new OpeningExplorerError(0, 'invalid_collection',
        `collection must be one of ${COLLECTIONS.join(', ')}`);
    }
    const hasMoves = moves !== undefined;
    const hasFen = fen !== undefined;
    if (hasMoves === hasFen) {
      throw new OpeningExplorerError(0, 'invalid_position_arguments',
        'pass exactly one of moves or fen');
    }
    const params = new URLSearchParams({ collection });
    if (hasMoves) {
      params.set('moves', Array.isArray(moves) ? moves.join(',') : moves);
    } else {
      params.set('fen', fen);
    }
    return this.#getJson(`/v1/explorer/position?${params.toString()}`, { cache: true });
  }

  /**
   * GET /v1/collections/{collection}/games/{id}: one game as PGN text.
   * Fails with status 404 when retrieval is disabled on the server.
   *
   * @param {'masters-otb'|'elite-online'} collection
   * @param {string} id Decimal game ID as a string, for example "27289182012770".
   * @returns {Promise<string>} PGN text.
   */
  async game(collection, id) {
    if (!COLLECTIONS.includes(collection)) {
      throw new OpeningExplorerError(0, 'invalid_collection',
        `collection must be one of ${COLLECTIONS.join(', ')}`);
    }
    if (typeof id !== 'string' || !/^\d+$/.test(id) || BigInt(id) > MAX_GAME_ID) {
      throw new OpeningExplorerError(0, 'invalid_game_id',
        'id must be a decimal string no larger than 18446744073709551615');
    }
    const response = await this.#send(
      `/v1/collections/${collection}/games/${id}`, { accept: 'application/x-chess-pgn' });
    return response.text();
  }

  async #getJson(path, { cache = false } = {}) {
    const url = this.baseUrl + path;
    const cached = cache ? this.etagCache.get(url) : undefined;
    const headers = {};
    if (cached) {
      headers['if-none-match'] = cached.etag;
    }
    const response = await this.#send(path, headers);
    if (response.status === 304 && cached) {
      return cached.body;
    }
    const body = await response.json();
    const etag = response.headers.get('etag');
    if (cache && etag) {
      this.etagCache.set(url, { etag, body });
    }
    return body;
  }

  async #send(path, extraHeaders = {}) {
    let response;
    try {
      response = await this.fetchImpl(this.baseUrl + path, {
        headers: { accept: 'application/json', ...extraHeaders },
        signal: AbortSignal.timeout(this.timeoutMs),
      });
    } catch (error) {
      throw new OpeningExplorerError(0, 'network_error', String(error?.message ?? error));
    }
    if (response.ok || response.status === 304) {
      return response;
    }
    throw await toApiError(response);
  }
}

// Error bodies use {"error":{"code","message"}}, including framework rejections
// such as unknown routes or timeouts. A body that is not JSON (for example one
// from a proxy in front of the server) falls back to the generic code http_error.
async function toApiError(response) {
  let code = 'http_error';
  let message = response.statusText || `HTTP ${response.status}`;
  try {
    const text = await response.text();
    if (text) {
      const parsed = JSON.parse(text);
      if (parsed?.error) {
        code = parsed.error.code ?? code;
        message = parsed.error.message ?? message;
      }
    }
  } catch {
    // Non-JSON error body (for example one returned by a proxy).
  }
  return new OpeningExplorerError(response.status, code, message);
}
