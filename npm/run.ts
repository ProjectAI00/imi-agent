#!/usr/bin/env bun
"use strict";

import * as https from "https";
import { execSync, spawnSync } from "child_process";
import { existsSync, mkdirSync, chmodSync, unlinkSync, createWriteStream, copyFileSync, readFileSync, writeFileSync } from "fs";
import { basename, join } from "path";
import { homedir, tmpdir } from "os";
import { IncomingMessage } from "http";
const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url).pathname, "utf8")) as { version: string };
const VERSION: string = pkg.version;
const REPO = "ProjectAI00/imi-agent";
const BIN_DIR = join(homedir(), ".local", "bin");
const BIN = join(BIN_DIR, "imi");

function getTarget(): string {
  const { platform, arch } = process;
  if (platform === "darwin" && arch === "arm64") return "aarch64-apple-darwin";
  if (platform === "darwin" && arch === "x64") return "x86_64-apple-darwin";
  if (platform === "linux" && arch === "x64") return "x86_64-unknown-linux-musl";
  if (platform === "linux" && arch === "arm64") return "aarch64-unknown-linux-musl";
  console.error(`Unsupported platform: ${platform} ${arch}`);
  process.exit(1);
}

function fetch(url: string, dest: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const file = createWriteStream(dest);
    const req = (u: string) =>
      https.get(u, (res: IncomingMessage) => {
        if (res.statusCode === 301 || res.statusCode === 302) {
          return req(res.headers.location as string);
        }
        if (res.statusCode !== 200) {
          reject(new Error(`HTTP ${res.statusCode} for ${u}`));
          return;
        }
        res.pipe(file);
        file.on("finish", () => file.close(resolve as () => void));
      }).on("error", reject);
    req(url);
  });
}

async function main(): Promise<void> {
  const target = getTarget();
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/imi-${target}.tar.gz`;
  const tmp = join(tmpdir(), `imi-${Date.now()}.tar.gz`);

  if (existsSync(BIN)) {
    try {
      const installed = execSync(`${BIN} --version 2>/dev/null`, { encoding: "utf8" }).trim();
      if (installed.includes(VERSION)) {
        console.log(`imi ${VERSION} already installed`);
        runInit();
        return;
      }
    } catch {}
  }

  process.stdout.write(`Installing imi v${VERSION} for ${target}... `);
  await fetch(url, tmp);
  mkdirSync(BIN_DIR, { recursive: true });
  execSync(`tar -xzf "${tmp}" -C "${BIN_DIR}"`, { stdio: "pipe" });
  chmodSync(BIN, 0o755);
  unlinkSync(tmp);
  console.log("done");

  // Install hankweave (execution engine)
  try {
    execSync("hankweave --version 2>/dev/null || bunx hankweave --version 2>/dev/null", { stdio: "pipe", timeout: 5000 });
  } catch {
    process.stdout.write("Installing hankweave... ");
    try {
      execSync("npm install -g hankweave", { stdio: "pipe", timeout: 30000 });
      console.log("done");
    } catch {
      console.log("skipped (install manually: npm install -g hankweave)");
    }
  }

  const inPath = (process.env.PATH || "").split(":").includes(BIN_DIR);
  if (!inPath) {
    console.log(`\nAdd to your shell config:\n  export PATH="$HOME/.local/bin:$PATH"\n`);
  }

  runInit();
}

function installSkills(): void {
  const skillsDir = join(import.meta.dir, "skills", "imi");
  const skillSrc = join(skillsDir, "SKILL.md");
  if (!existsSync(skillSrc)) return;

  // Sub-files that accompany SKILL.md in agents that support multi-file skill dirs
  const subFiles = ["ops-mode.md", "plan-mode.md", "execute-mode.md", "ai-voice.md"];
  const skillFiles = [skillSrc, ...subFiles.map(f => join(skillsDir, f))].filter(existsSync);
  const alwaysOnInstructions = buildAlwaysOnInstructions();

  // Agents that support skill sub-directories: install each file separately
  const multiFileTargets: { name: string; dir: string }[] = [
    { name: "GitHub Copilot CLI", dir: join(homedir(), ".copilot", "skills", "imi") },
    { name: "Claude Code",        dir: join(homedir(), ".claude",  "skills", "imi") },
  ];

  // Agents that use a single flat file: install compact bootstrap content
  const singleFileTargets: { name: string; dir: string; filename: string }[] = [
    { name: "Cursor",           dir: join(homedir(), ".cursor",   "rules"),          filename: "imi.md"          },
    { name: "OpenCode",         dir: join(homedir(), ".opencode", "instructions"),  filename: "imi-session.md"  },
    { name: "OpenAI Codex",     dir: join(homedir(), ".codex"),                     filename: "instructions.md" },
  ];

  // Flat always-on instruction files must stay small because tools inject them
  // before the agent can choose to fetch more context. Keep full docs as sidecars.
  const sidecarDocTargets: { name: string; dir: string }[] = [
    { name: "Cursor IMI docs",       dir: join(homedir(), ".cursor",   "skills", "imi") },
    { name: "OpenCode IMI docs",     dir: join(homedir(), ".opencode", "skills", "imi") },
    { name: "OpenAI Codex IMI docs", dir: join(homedir(), ".codex",    "skills", "imi") },
  ];

  const installed: string[] = [];
  const skipped: string[] = [];

  for (const { name, dir } of multiFileTargets) {
    const agentRoot = join(dir, "..", "..");
    if (!existsSync(agentRoot)) { skipped.push(name); continue; }
    mkdirSync(dir, { recursive: true });
    writeSkillFiles(skillsDir, dir, skillFiles);
    installed.push(name);
  }

  for (const { name, dir, filename } of singleFileTargets) {
    const agentRoot = join(dir, "..", "..");
    if (!existsSync(agentRoot)) { skipped.push(name); continue; }
    mkdirSync(dir, { recursive: true });
    writeAlwaysOnFile(join(dir, filename), alwaysOnInstructions);
    installed.push(name);
  }

  for (const { name, dir } of sidecarDocTargets) {
    const agentRoot = join(dir, "..", "..");
    if (!existsSync(agentRoot)) continue;
    mkdirSync(dir, { recursive: true });
    writeSkillFiles(skillsDir, dir, skillFiles);
    installed.push(name);
  }

  if (installed.length > 0) {
    console.log(`\nAgent skills installed into: ${installed.join(", ")}`);
    console.log(`Always-on agent instructions stay compact; full IMI docs are installed as on-demand sidecars.`);
  }

  // Also write AGENTS.md and CLAUDE.md in the current working directory if a
  // .imi/ folder exists. These are always-on in many tools, so keep them small.
  const cwd = process.cwd();
  if (existsSync(join(cwd, ".imi"))) {
    writeAlwaysOnFile(join(cwd, "AGENTS.md"), alwaysOnInstructions);
    writeAlwaysOnFile(join(cwd, "CLAUDE.md"), alwaysOnInstructions);

    // GitHub Copilot CLI custom agent profile (.github/agents/imi.agent.md)
    const agentSrc = join(skillsDir, "imi.agent.md");
    if (existsSync(agentSrc)) {
      const agentsDir = join(cwd, ".github", "agents");
      mkdirSync(agentsDir, { recursive: true });
      writeFileSync(join(agentsDir, "imi.agent.md"), readFileSync(agentSrc, "utf8"));
    }
  }

  registerClaudePlugin();
}

function buildAlwaysOnInstructions(): string {
  return `---
description: IMI bootstrap for persistent product state
alwaysApply: true
---

# IMI Bootstrap

IMI is the project state layer for goals, tasks, decisions, lessons, and direction. Keep this always-on prompt small; load the full mode docs only when the task needs them.

## Start Every Session

If the workspace has a .imi directory, or the user asks about status, goals, tasks, priorities, decisions, progress, or where work left off, run:

\`\`\`bash
imi context
\`\`\`

Use the output as project state. Do not inspect .imi files directly.

## Route By Intent

- Ops/status/decision conversations: use \`imi context\`, \`imi plan\`, \`imi check\`, or \`imi think\` as needed.
- Planning work: create goals/tasks with \`why\`, \`success_signal\`, \`--acceptance-criteria\`, and \`--relevant-files\`.
- Execution work: follow the task spec, verify acceptance criteria, then run \`imi complete <task_id> "rich summary"\`.
- Durable decisions or discoveries: record them with \`imi decide "what" "why"\` or \`imi log "note"\`.

## Full Docs On Demand

The detailed IMI docs are installed as sidecar files so they do not inflate every prompt:

- \`SKILL.md\` — activation contract and command quick reference.
- \`ops-mode.md\` — status, direction, and decision conversations.
- \`plan-mode.md\` — writing high-quality goals and task specs.
- \`execute-mode.md\` — executing task specs and completing work.
- \`ai-voice.md\` — writing durable IMI summaries, logs, and lessons.

Look for them in the agent skill directory, commonly \`~/.claude/skills/imi\`, \`~/.copilot/skills/imi\`, \`~/.cursor/skills/imi\`, \`~/.opencode/skills/imi\`, or \`~/.codex/skills/imi\`. Load only the relevant file for the current mode.

## Hard Constraints

- Treat IMI as state, not execution. IMI records what should happen, what happened, and what was learned.
- Do not silently reduce task scope or rewrite acceptance criteria to match a smaller implementation.
- Prefer the repository's existing patterns and keep edits scoped to the user's request.
- Keep always-on instructions under 10k tokens; do not paste full mode manuals into AGENTS.md, CLAUDE.md, or global rule files.
`;
}

function writeSkillFiles(skillsDir: string, targetDir: string, files: string[]): void {
  for (const file of files) {
    const targetName = file === join(skillsDir, "SKILL.md") ? "SKILL.md" : basename(file);
    writeFileSync(join(targetDir, targetName), readFileSync(file, "utf8"));
  }
}

function writeAlwaysOnFile(path: string, content: string): void {
  const maxAlwaysOnChars = 40_000; // Roughly 10k tokens at 4 chars/token.
  if (content.length > maxAlwaysOnChars) {
    throw new Error(`refusing to write oversized always-on IMI instructions (${content.length} chars): ${path}`);
  }
  writeFileSync(path, content);
}

function registerClaudePlugin(): void {
  const claudePluginsDir = join(homedir(), ".claude", "plugins");
  const knownFile = join(claudePluginsDir, "known_marketplaces.json");

  if (!existsSync(claudePluginsDir)) return;

  // Read existing marketplaces
  let known: Record<string, unknown> = {};
  try { known = JSON.parse(readFileSync(knownFile, "utf8")); } catch {}

  if (known["imi"]) return; // already registered

  const installLocation = join(claudePluginsDir, "marketplaces", "imi");

  // Clone the repo so Claude Code can read the plugin manifest
  if (!existsSync(installLocation)) {
    try {
      execSync(
        `git clone --depth 1 https://github.com/ProjectAI00/imi-agent "${installLocation}"`,
        { stdio: "pipe", timeout: 15000 }
      );
    } catch {
      // git unavailable or no network — register the source URL anyway
    }
  }

  known["imi"] = {
    source: { source: "github", repo: "ProjectAI00/imi-agent" },
    installLocation,
    lastUpdated: new Date().toISOString(),
  };
  writeFileSync(knownFile, JSON.stringify(known, null, 2));
  console.log(`  Claude Code: marketplace registered → run /plugin install imi to activate`);
}

function runInit(): void {
  installSkills();
  const result = spawnSync(BIN, ["init"], { stdio: "inherit" });
  process.exit(result.status ?? 0);
}

main().catch((err: Error) => {
  console.error("\nInstall failed:", err.message);
  console.error(`Manual install: curl -fsSL https://useimi.com/install | bash`);
  process.exit(1);
});
