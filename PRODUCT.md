# Product

<!-- impeccable:product-schema 1 -->

## Platform

adaptive

## Users

Developers operating one primary coding agent and parallel specialist agents from a terminal.

## Product Purpose

SERAPH is a token-efficient agent harness: models orchestrate tools and agents programmatically while large state remains outside conversation context.

## Positioning

SERAPH combines a persistent Python control environment, lazy capabilities, durable coordination, and native parallel Codex agents in a standalone Rust terminal application.

## Operating Context

SERAPH runs inside a project checkout. The user signs in through Codex, chats with the primary agent, watches delegated work, and inspects all active agents without leaving the terminal.

## Capabilities and Constraints

- Rust and Ratatui application using the installed Codex app-server.
- Grok Build is the binding reference for the primary TUI shell and interaction quality.
- Prime Agent is the binding reference for the down-arrow All Agents view.
- Agent and tool state must stay out of model context until requested.
- Mutations should be atomic and reversible where practical.

## Brand Commitments

The product name is SERAPH: Stateful Execution Runtime for Agentic Programmatic Harness. Its UI is approximately 95% Grok Build and 5% Prime Agent's multi-agent navigation, adapted to SERAPH rather than presented as either donor product.

## Evidence on Hand

The repository contains working Codex authentication/chat, a persistent Python kernel, native parallel agents, and a durable SQLite coordination board.

## Product Principles

- Compute locally and spend tokens on reasoning.
- Load tools, skills, memory, and artifacts only when needed.
- Make concurrent work visible and ownership unambiguous.
- Reuse proven crates and permissively licensed donor code before inventing infrastructure.
