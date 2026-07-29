# Graph Report - vrt  (2026-07-29)

## Corpus Check
- 178 files · ~77,032 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 179 nodes · 437 edges · 15 communities (14 shown, 1 thin omitted)
- Extraction: 100% EXTRACTED · 0% INFERRED · 0% AMBIGUOUS · INFERRED: 1 edges (avg confidence: 0.5)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `41656d38`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- vrt_flow_integration.rs
- render_flow_integration.rs
- service/src/builds.rs
- api.$.ts
- .start
- entity/src/builds.rs
- StoryRenderer
- browser.rs
- .render_story
- String
- .parse
- post-commit
- ci.rs
- post-checkout

## God Nodes (most connected - your core abstractions)
1. `Fixture` - 12 edges
2. `Fixture` - 12 edges
3. `storybook_bundle_is_rendered_server_side_and_compared()` - 11 edges
4. `create_build()` - 10 edges
5. `StoryRenderer` - 10 edges
6. `setup()` - 10 edges
7. `vrt_full_flow_from_first_build_to_stable_baseline()` - 10 edges
8. `approve_build()` - 9 edges
9. `renders_a_story_to_a_png_with_the_requested_viewport()` - 9 edges
10. `a_story_that_renders_nothing_still_produces_a_screenshot()` - 9 edges

## Surprising Connections (you probably didn't know these)
- `create_build()` --references--> `BuildMode`  [EXTRACTED]
  apps/backend/crates/service/src/builds.rs → apps/backend/crates/entity/src/builds.rs
- `transition()` --references--> `BuildStatus`  [EXTRACTED]
  apps/backend/crates/service/src/builds.rs → apps/backend/crates/entity/src/builds.rs

## Import Cycles
- 1-file cycle: `apps/backend/crates/entity/src/builds.rs -> apps/backend/crates/entity/src/builds.rs`

## Communities (15 total, 1 thin omitted)

### Community 0 - "vrt_flow_integration.rs"
Cohesion: 0.14
Nodes (29): assert_completed_at_is_stamped(), baseline_entry_count(), build_can_be_fetched_by_project_scoped_number(), build_id_of(), counts(), dump_apalis_state(), duplicate_screenshot_name_is_conflict(), encode() (+21 more)

### Community 1 - "render_flow_integration.rs"
Cohesion: 0.19
Nodes (21): a_bundle_without_an_index_fails_the_build_with_a_reason(), assert_completed_at_is_stamped(), build_id_of(), bundle_zip(), bundles_larger_than_the_default_body_limit_are_accepted(), chromium_or_skip(), Fixture, iframe_html() (+13 more)

### Community 2 - "service/src/builds.rs"
Cohesion: 0.26
Nodes (26): AppError, apply_counts(), approve_all_pending(), approve_build(), attach_storybook_bundle(), BuildCounts, count_builds(), create_build() (+18 more)

### Community 3 - "api.$.ts"
Cohesion: 0.24
Nodes (11): BodyTooLargeError, buildBackendUrl(), copyHeaders(), handler(), HOP_BY_HOP, LimitedReadableStream, limitReadableStream(), maxBodyBytesForPath() (+3 more)

### Community 4 - ".start"
Cohesion: 0.32
Nodes (7): a_story_error_signal_fails_fast_with_the_reason(), a_story_that_renders_nothing_still_produces_a_screenshot(), launching_a_missing_chromium_fails_fast(), renders_a_story_to_a_png_with_the_requested_viewport(), static_server_serves_the_bundle_over_loopback(), AsRef, Into

### Community 6 - "StoryRenderer"
Cohesion: 0.28
Nodes (6): StaticServer, StoryRenderer, Browser, Drop, JoinHandle, SocketAddr

### Community 7 - "browser.rs"
Cohesion: 0.36
Nodes (5): RenderOptions, write_fixture_bundle(), write_storybook_runtime_bundle(), Duration, Path

### Community 8 - ".render_story"
Cohesion: 0.43
Nodes (5): RenderError, Result, Vec, CdpError, Page

### Community 9 - "String"
Cohesion: 0.47
Nodes (6): discover_chromium(), playwright_chromium(), Option, String, story_url(), urlencode()

### Community 10 - ".parse"
Cohesion: 0.40
Nodes (4): Readiness, readiness_parses_probe_results(), Value, Self

### Community 11 - "post-commit"
Cohesion: 0.40
Nodes (4): post-commit script, GRAPHIFY_CHANGED, GRAPHIFY_REBUILD_LOG, PYTHONHASHSEED

### Community 12 - "ci.rs"
Cohesion: 0.83
Nodes (3): routes(), AppState, OpenApiRouter

### Community 13 - "post-checkout"
Cohesion: 0.50
Nodes (3): post-checkout script, GRAPHIFY_REBUILD_LOG, PYTHONHASHSEED

## Knowledge Gaps
- **2 isolated node(s):** `LimitedReadableStream`, `Route`
  These have ≤1 connection - possible missing edges or undocumented components.
- **1 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `RenderError` connect `.render_story` to `String`, `.start`, `browser.rs`?**
  _High betweenness centrality (0.049) - this node is a cross-community bridge._
- **Why does `RenderOptions` connect `browser.rs` to `String`, `.start`, `StoryRenderer`?**
  _High betweenness centrality (0.024) - this node is a cross-community bridge._
- **What connects `LimitedReadableStream`, `Route` to the rest of the system?**
  _2 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `vrt_flow_integration.rs` be split into smaller, more focused modules?**
  _Cohesion score 0.14126984126984127 - nodes in this community are weakly interconnected._