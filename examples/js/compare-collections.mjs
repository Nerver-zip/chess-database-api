// Compare one position across Masters OTB and Elite Online.
//
// The two collections are kept separate on purpose. Each row shows one
// collection's own counts. The script never sums counts across collections
// or averages their ratings, because FIDE Elo (over-the-board) and Lichess
// Glicko-2 (online) are different rating systems.
//
// Usage:
//   OPENING_EXPLORER_URL=http://127.0.0.1:8080 node examples/js/compare-collections.mjs [moves]

import { OpeningExplorerClient, OpeningExplorerError } from './opening-explorer.mjs';

const baseUrl = process.env.OPENING_EXPLORER_URL ?? 'http://127.0.0.1:8080';
const moves = process.argv[2] ?? 'e4,e5,Nf3';
const client = new OpeningExplorerClient({ baseUrl });

const loaded = (await client.collections()).map((c) => c.collection);

console.log(`Position after ${moves}, one row per collection (counts are not combined):\n`);
console.log('collection     games  White%  Draw%  Black%  top reply (games)');

for (const collection of ['masters-otb', 'elite-online']) {
  if (!loaded.includes(collection)) {
    console.log(`${collection.padEnd(14)} not loaded on this server`);
    continue;
  }
  try {
    const p = await client.position({ collection, moves });
    const top = p.moves[0];
    const topText = top ? `${top.san} (${top.total_games})` : 'none (no indexed continuation)';
    console.log(
      `${collection.padEnd(14)} ${String(p.total_games).padStart(7)} ` +
      `${p.white_win_pct.toFixed(1).padStart(6)} ${p.draw_pct.toFixed(1).padStart(6)} ` +
      `${p.black_win_pct.toFixed(1).padStart(7)}  ${topText}`,
    );
  } catch (error) {
    if (error instanceof OpeningExplorerError) {
      console.log(`${collection.padEnd(14)} error ${error.status} (${error.code})`);
    } else {
      throw error;
    }
  }
}

console.log('\nRead each row against its own depth (see /v1/collections).');
console.log('A zero row can mean the position is beyond the indexed depth; see docs/API_REFERENCE.md.');
