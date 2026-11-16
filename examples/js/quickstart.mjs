// Quickstart: read the loaded collections, query a position by moves and by
// FEN, and print its next-move statistics.
//
// Run with Node.js 18 or later:
//   OPENING_EXPLORER_URL=http://127.0.0.1:8080 node examples/js/quickstart.mjs [moves]
//
// The optional argument is a comma-separated SAN or UCI move list, for example
// "d4,d5,c4". It defaults to "e4,e5,Nf3".

import { OpeningExplorerClient, OpeningExplorerError } from './opening-explorer.mjs';

const baseUrl = process.env.OPENING_EXPLORER_URL ?? 'http://127.0.0.1:8080';
const moves = process.argv[2] ?? 'e4,e5,Nf3';
const client = new OpeningExplorerClient({ baseUrl });

function percent(value) {
  return `${value.toFixed(1).padStart(5)}%`;
}

function printPosition(position) {
  console.log(`  collection:    ${position.collection}`);
  console.log(`  dataset:       ${position.dataset_version}`);
  console.log(`  position key:  ${position.position_key}`);
  console.log(`  FEN:           ${position.fen}`);
  console.log(`  games:         ${position.total_games}`);
  console.log(`  White wins:    ${position.white_wins} (${percent(position.white_win_pct)})`);
  console.log(`  Draws:         ${position.draws} (${percent(position.draw_pct)})`);
  console.log(`  Black wins:    ${position.black_wins} (${percent(position.black_win_pct)})`);
  console.log('  Next moves (most played first):');
  console.log('    move    games   White     Draw     Black');
  for (const move of position.moves.slice(0, 8)) {
    console.log(
      `    ${move.san.padEnd(6)} ${String(move.total_games).padStart(7)} ` +
      `${percent(move.white_win_pct)} ${percent(move.draw_pct)} ${percent(move.black_win_pct)}`,
    );
  }
}

try {
  const health = await client.health();
  console.log(`Server ${baseUrl} is ${health.status} (software ${health.version}).`);

  const collections = await client.collections();
  console.log('Loaded collections:');
  for (const c of collections) {
    console.log(`  ${c.collection}: ${c.total_games} games, depth ${c.cpi_depth_plies ?? 'n/a'} plies`);
  }

  const target = collections[0]?.collection;
  if (!target) {
    throw new Error('the server has no loaded collections');
  }

  console.log(`\nPosition after ${moves} in ${target} (by moves):`);
  printPosition(await client.position({ collection: target, moves }));

  // Asking for the FEN of the same position returns the same statistics.
  const byMoves = await client.position({ collection: target, moves });
  const fen = byMoves.fen;
  const byFen = await client.position({ collection: target, fen });
  const same = byMoves.position_key === byFen.position_key && byMoves.total_games === byFen.total_games;
  console.log(`\nFEN lookup of the same position matches: ${same}`);
} catch (error) {
  if (error instanceof OpeningExplorerError) {
    console.error(`API error ${error.status} (${error.code}): ${error.message}`);
  } else {
    console.error(error);
  }
  process.exitCode = 1;
}
