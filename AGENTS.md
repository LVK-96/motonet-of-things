# AGENTS

## Project Overview
This is a Rust-based firmware project for ESP32, focusing on IoT sensor data collection and display.
- **Goal**: Ingest 433MHz radio data (Rubicson sensors), display it on an OLED screen, and publish via MQTT.
- **Maintainer**: Leo (Single contributor).
- **Style**: Casual but descriptive commit messages.

## Architecture
The system uses `embassy` for async task management on the ESP32.
- **Runtime**: `embassy-executor` on `esp-rtos` (via `esp-hal`).
- **Communication**: `embassy-sync` channels pass data between tasks.
  - `radio_433_rx`: Ingests raw radio data.
  - `mqtt_task`: Publishes telemetry.
  - `display_task`: Updates the UI.
- **Hardware Abstraction**: `esp-hal` for peripheral access.

## Structure
- `firmware/`: Main application binary (`no_std`).
- `crates/rubicson`: Driver for Rubicson 433MHz temperature sensors.
- `crates/eu-dst`: European Daylight Saving Time calculation.
- `scripts/`: Utility scripts (e.g., `mqtt-test.sh`).

## Development Workflow

### Build & Run
- **Flash & Monitor**: Run from `firmware/` directory:
  ```bash
  cargo run --release
  ```
  This uses `espflash` to flash and monitor the device.

### Testing
- **Unit Tests (Host)**: Run from root or crate directory:
  ```bash
  cargo test
  ```
  Note: `firmware` crate is `no_std` and difficult to test on host. Logic is moved to helper crates (like `rubicson`) for host-side testing.
- **Integration Tests**: `scripts/mqtt-test.sh` for verifying MQTT flows locally.

## Key Considerations for Agents
1.  **Environment**: `no_std` context for firmware. Standard library is available for testing helper crates.
2.  **Testing**: Always verify logic changes with `cargo test` in the relevant crate before proposing fixes.
3.  **Conventions**:
    - Use `cargo fmt` to maintain style.
    - Run `cargo clippy` to check for lints. Note: Run `cargo clippy` in `firmware/` for the target, and `cargo clippy -p <crate>` for host crates.
    - Commit messages should be "Action: Details" (e.g., "Fix decode logic for negative temps").
4.  **Known Issues**:
    - **Temperature Encoding**: The `rubicson` crate uses 12-bit two's complement. Be careful with sign extension. Negative temperatures (e.g., -10.5°C) correspond to raw values with high nibbles (e.g., 0xFxx). Always test with negative values.


<skills_system priority="1">

## Available Skills

<!-- SKILLS_TABLE_START -->
<usage>
When users ask you to perform tasks, check if any of the available skills below can help complete the task more effectively. Skills provide specialized capabilities and domain knowledge.

How to use skills:
- Invoke: `npx openskills read <skill-name>` (run in your shell)
  - For multiple: `npx openskills read skill-one,skill-two`
- The skill content will load with detailed instructions on how to complete the task
- Base directory provided in output for resolving bundled resources (references/, scripts/, assets/)

Usage notes:
- Only use skills listed in <available_skills> below
- Do not invoke a skill that is already loaded in your context
- Each skill invocation is stateless
</usage>

<available_skills>

<skill>
<name>brainstorming</name>
<description>"You MUST use this before any creative work - creating features, building components, adding functionality, or modifying behavior. Explores user intent, requirements and design before implementation."</description>
<location>project</location>
</skill>

<skill>
<name>dispatching-parallel-agents</name>
<description>Use when facing 2+ independent tasks that can be worked on without shared state or sequential dependencies</description>
<location>project</location>
</skill>

<skill>
<name>doc-coauthoring</name>
<description>Guide users through a structured workflow for co-authoring documentation. Use when user wants to write documentation, proposals, technical specs, decision docs, or similar structured content. This workflow helps users efficiently transfer context, refine content through iteration, and verify the doc works for readers. Trigger when user mentions writing docs, creating proposals, drafting specs, or similar documentation tasks.</description>
<location>project</location>
</skill>

<skill>
<name>executing-plans</name>
<description>Use when you have a written implementation plan to execute in a separate session with review checkpoints</description>
<location>project</location>
</skill>

<skill>
<name>finishing-a-development-branch</name>
<description>Use when implementation is complete, all tests pass, and you need to decide how to integrate the work - guides completion of development work by presenting structured options for merge, PR, or cleanup</description>
<location>project</location>
</skill>

<skill>
<name>pdf</name>
<description>Comprehensive PDF manipulation toolkit for extracting text and tables, creating new PDFs, merging/splitting documents, and handling forms. When Claude needs to fill in a PDF form or programmatically process, generate, or analyze PDF documents at scale.</description>
<location>project</location>
</skill>

<skill>
<name>receiving-code-review</name>
<description>Use when receiving code review feedback, before implementing suggestions, especially if feedback seems unclear or technically questionable - requires technical rigor and verification, not performative agreement or blind implementation</description>
<location>project</location>
</skill>

<skill>
<name>requesting-code-review</name>
<description>Use when completing tasks, implementing major features, or before merging to verify work meets requirements</description>
<location>project</location>
</skill>

<skill>
<name>subagent-driven-development</name>
<description>Use when executing implementation plans with independent tasks in the current session</description>
<location>project</location>
</skill>

<skill>
<name>systematic-debugging</name>
<description>Use when encountering any bug, test failure, or unexpected behavior, before proposing fixes</description>
<location>project</location>
</skill>

<skill>
<name>test-driven-development</name>
<description>Use when implementing any feature or bugfix, before writing implementation code</description>
<location>project</location>
</skill>

<skill>
<name>using-git-worktrees</name>
<description>Use when starting feature work that needs isolation from current workspace or before executing implementation plans - creates isolated git worktrees with smart directory selection and safety verification</description>
<location>project</location>
</skill>

<skill>
<name>using-superpowers</name>
<description>Use when starting any conversation - establishes how to find and use skills, requiring Skill tool invocation before ANY response including clarifying questions</description>
<location>project</location>
</skill>

<skill>
<name>verification-before-completion</name>
<description>Use when about to claim work is complete, fixed, or passing, before committing or creating PRs - requires running verification commands and confirming output before making any success claims; evidence before assertions always</description>
<location>project</location>
</skill>

<skill>
<name>writing-plans</name>
<description>Use when you have a spec or requirements for a multi-step task, before touching code</description>
<location>project</location>
</skill>

<skill>
<name>writing-skills</name>
<description>Use when creating new skills, editing existing skills, or verifying skills work before deployment</description>
<location>project</location>
</skill>

</available_skills>
<!-- SKILLS_TABLE_END -->

</skills_system>
