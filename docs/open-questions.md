# Open questions

Ranked by how much they could change the primitives. Close each with a link to a decision or test result.

1. **How do extension views render?** Declarative, terminal program, hybrid, or a mix. Test S3.
2. **When does mid-turn input take effect in each runtime, and which Claude Code route should the adapter use?** S1.
3. **How fine-grained should capabilities be** before extensions fill up with special cases? S2.
4. **Agent bridge:** does it reach the core over a local socket or through its terminal? How does it find the core
   when the backend is remote?
5. **Where prim's own state lives** (layouts, extension namespaces): a prim store, the backend's `store.docs`, or both.
6. **Workspace authority:** grant granularity for agents changing layouts, commands and state (cooperative, D8).
7. **One layout on clients of different sizes:** smallest client, owner sets size, or per-client rendering.
8. **Extension lifecycle:** crash budget, namespace schema versions, rules for extension-owned terminals. E0.
9. **Core protocol** shape, transport and schema (candidate rules in `research/protocols-and-ui.md`).
10. **Embedded VT:** alacritty_terminal vs libghostty-vt.
11. **How agents call prim** (message agents, change the workspace): CLI, MCP server, or library, with reliable caller
    identity (D8).
12. **Agent-to-agent addressing:** node vs durable agent identity vs execution (vqro lessons). Extension concern; core
    IDs must support it.
13. **Second backend:** tmux (ubiquitous, lossy status) or Zellij (OSC 7501 and library mode since 0.46, unverified).
14. **Licensing and terms**, for Legal Counsel: pi_agent_rust's rider, Rex third-party-client terms, automating Claude
    Code, prim's own licence before publishing.
