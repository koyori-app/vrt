/**
 * Create the e2e database if it does not exist yet.
 *
 * `migration fresh` drops every table, so the e2e run must never touch the dev
 * database. Creating it here (instead of shelling out to psql) keeps the only
 * host requirement at "node + a reachable Postgres".
 */
import { Client } from "pg";

const url = new URL(process.env.DATABASE_URL ?? "postgresql://vrt:vrt@localhost:5433/vrt_e2e");
const database = decodeURIComponent(url.pathname.replace(/^\//, ""));

if (!database) {
  throw new Error(`DATABASE_URL has no database name: ${url}`);
}

// Connect to the always-present maintenance database to issue CREATE DATABASE.
const adminUrl = new URL(url);
adminUrl.pathname = "/postgres";

const client = new Client({ connectionString: adminUrl.toString() });
await client.connect();

try {
  const existing = await client.query("SELECT 1 FROM pg_database WHERE datname = $1", [database]);
  if (existing.rowCount === 0) {
    // Identifiers cannot be parameterised; the name comes from our own config,
    // and the quote-doubling keeps a stray quote from breaking out.
    await client.query(`CREATE DATABASE "${database.replaceAll('"', '""')}"`);
    console.log(`created database ${database}`);
  } else {
    console.log(`database ${database} already exists`);
  }
} finally {
  await client.end();
}
