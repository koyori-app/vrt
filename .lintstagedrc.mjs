export default {
  "apps/frontend/**/*.{ts,tsx,js,jsx,mjs,cjs}": () =>
    "bash -c \"cd apps/frontend && pnpm fmt\"",
  "apps/backend/**/*.rs": () => [
    "bash -c \"cd apps/backend && cargo fmt --all\"",
    "bash -c \"cd apps/backend && cargo clippy --workspace --all-targets -- -D warnings\"",
  ],
};
