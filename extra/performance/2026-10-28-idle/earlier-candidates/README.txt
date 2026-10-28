These datasets are supplemental and are NOT pooled into the final background tables.
final-* and throughput use the earlier candidate (before compact row indices / dynamic GPU allocation).
dynamic-ascii is a one-pair foreground smoke of the final candidate.
verified-ascii contains only completed cases from an interrupted foreground attempt, not a complete paired series. External keyboard input invalidated the following case; its contents are intentionally not archived.
GPU reclaim experiments were rejected: CGLReclaimResources crashed its isolated test app; finish/swap limit/allocator relief/opaque/vsync showed no consistent benefit and were not adopted.
history-shared-cache-warm-pages.jsonl has cold search but page/reflow caches shared with the prior search through Arc; its page/reflow timings are not valid cold-operation measurements. A corrected benchmark independently prepares each operation.
background-ascii and background-cjk are complete three-pair final-binary runs with the initial no-activation shim; background-ansi is incomplete. The later input-isolated shim has a different recorded hash; the final table uses only isolated-* runs.
before-cold-reflow-* uses the 353c8fbd candidate before mutable access was changed to reuse a shared expansion. Final numerical tables use the rebuilt candidate after this correction.
final-first-frame-attempt.csv records a rejected native attempt with 218 scroll frames and no local input GPU acknowledgements; the independent retry completed 240 frames and all 100 input samples. No assertion was relaxed.
