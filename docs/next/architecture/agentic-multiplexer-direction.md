# Agentic multiplexer direction

Status: exploration note

Vqro is exploring an open, extensible agentic multiplexer: a terminal-native environment in which humans, agents, tools, and automation can share work without being forced into one provider, one workflow, or one hosted service.

This is a direction, not a settled product specification. It describes hypotheses worth preserving while Vqro is built, not claims that the current implementation already satisfies them. Agentic work is new enough that its durable interaction model is not yet known. Vqro should provide a useful opinionated experience while keeping that experience inspectable, replaceable, and open to alternatives.

## What Vqro multiplexes

A traditional terminal multiplexer routes processes, terminal input, output, and human focus. Vqro starts there and explores multiplexing additional concerns involved in agentic work:

- execution among terminals, agents, tools, and automation;
- responsibilities and work among participants;
- context and messages across boundaries;
- human attention, interruption, and escalation;
- authority to act or delegate; and
- claims, evidence, and accepted outcomes.

These concerns do not imply that one task model, hierarchy, or coordination policy belongs in the host. PTYs are canonical for execution and recovery, not automatically the source of truth for work, messaging, or authority. These concerns describe the problem space that extensions and users should be able to explore.

## A useful default, not a final ontology

The initial experience is aimed at a solo power user coordinating terminal-native work. It may use workspaces, delegated subtrees, structured messages, bounded authority, attention views, and evidence-based completion. That batteries-included reference composition should be useful without additional assembly, but it is not Vqro's core ontology.

It is also an experiment. Other extensions may organize the same underlying environment as a queue, graph, notebook, pipeline, peer group, or model not anticipated here. Vqro should not mistake its default workflow for the only valid shape of human-agent work.

The communication and responsibility structure of a group influences what it produces. Making that structure visible and replaceable may let users deliberately test different organizational forms rather than inheriting one from a closed agent product.

## Platform boundary

A working hypothesis is that a neutral host should preserve access and integrity when opinionated extensions are absent or broken. Likely host mechanisms include real terminal sessions, stable identities, lifecycle, persistence, recovery, capability enforcement, package execution, and public communication boundaries. The exact boundary remains open.

Product meaning and policy should live outside that neutral substrate wherever practical. First-party experiences should converge on the same public contracts available to independently developed extensions; inherited or transitional privileged paths are implementation state, not an intended architectural advantage. Being bundled or curated should describe discovery and distribution rather than grant additional execution authority. Public-contract compatibility makes a package eligible for documented review and admission; it does not itself grant capabilities, namespace ownership, or feature authority.

This boundary will be refined from working vertical slices rather than by designing a universal agent ontology in advance. Extension work should be bounded and event-driven, not coupled to terminal bytes, client count, or render frames.

## Extensible experience

Integrated extensions should primarily describe meaning and intent while the active client owns concrete rendering, theme, accessibility, focus, and input behavior. Collections is an early example: an extension can project labels, archive semantics, groups, and stable terminal references while Vqro renders and operates the terminals. These native documents are optional cached semantic summaries and controls for the multiplexer, not a requirement that extension applications use Vqro widgets.

Future integrated experiences may require richer host-rendered surfaces or workspace-level compositions. Their exact contracts are intentionally unsettled. Full applications may remain terminal-native. Vqro should not infer their semantics by scraping terminal cells or grow a general embedded-widget runtime without separate evidence and authority review. A tool that needs unrestricted interface control can run as an ordinary application in a real terminal; a sandboxed package does not thereby gain process-spawning or terminal-control authority.

The built-in TUI is the first product surface, not necessarily the only client. Shared runtime contracts should avoid making presentation choices mandatory for future clients.

## Open environment

Vqro should be local-first, useful without a required cloud or model provider, and capable of attaching to remote work. User state should remain under user control and be portable through open contracts where those contracts mature.

Agents and extensions have different trust boundaries. Sandboxed extension packages should receive explicit host capabilities. A terminal agent is a process the user chose to run and ordinarily has that process's operating-system authority; Vqro should expose its platform-visible activity and delegated authority without claiming containment or complete observation it does not provide.

Model intelligence may improve routing, summarization, or policy, but no particular model should be required for the basic environment. Intelligent layers should consume public state and remain replaceable.

## Relationship to terminal backends

Vqro currently contains Herdr-derived terminal/session runtime code; it does not yet sit above a separate, replaceable Herdr backend. A future neutral backend boundary may allow another implementation to participate without rebuilding the product experience around backend internals.

This is a direction rather than a near-term compatibility promise. Real PTYs, provider-independent reachability, and recovery remain more important than prematurely generalizing a backend interface.

## What this note does not decide

This note does not freeze:

- a universal task, message, actor, or organization schema;
- the final boundary between neutral host services and extension-owned services;
- one hierarchy, autonomy model, or attention policy;
- a complete UI toolkit or workspace-lens protocol;
- multi-human collaboration or synchronization semantics;
- a required intelligent router or model provider; or
- the eventual business or governance structure of the ecosystem.

Vqro is not intended to replace the user's operating-system distribution, editor, or existing terminal tools. It should compose with them. Its primitives may eventually support highly autonomous organizations, but that is an emergent possibility to test—not a present claim of autonomous operation.

## Useful tests of the direction

Two useful tests, rather than a complete definition of success, are:

1. whether a solo user can rely on Vqro for substantial daily multi-agent work without losing direct access to terminals or source evidence; and
2. whether an independently developed extension can add or replace meaningful workflow behavior using public contracts rather than privileged host paths.

Extension externalization, neutral recovery, and first-party/third-party contract parity support those tests. They are not the final product by themselves.
