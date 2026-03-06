#!/usr/bin/env bun
"use strict";

import * as https from "https";
import { execSync, spawnSync } from "child_process";
import { existsSync, mkdirSync, chmodSync, unlinkSync, createWriteStream, copyFileSync, readFileSync, writeFileSync } from "fs";
import { join } from "path";
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
    execSync("hankweave --version 2>/dev/null || bunx hankweave --version 2>/dev/null", { stdio: "pipe" });
  } catch {
    process.stdout.write("Installing hankweave... ");
    try {
      execSync("npm install -g hankweave", { stdio: "pipe" });
      console.log("done");
    } catch {
      console.log("skipped (install manually: npm install -g hankweave)");
    }
  }

  // Install entire (commit tracking + session verification)
  try {
    execSync("entire version", { stdio: "pipe" });
  } catch {
    process.stdout.write("Installing entire... ");
    try {
      execSync("curl -fsSL https://entire.io/install.sh | bash", { stdio: "pipe", shell: true });
      console.log("done");
    } catch {
      console.log("skipped (install manually: curl -fsSL https://entire.io/install.sh | bash)");
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

  // For agents that use a single flat file, concatenate all content
  const allContent = [skillSrc, ...subFiles.map(f => join(skillsDir, f))]
    .filter(existsSync)
    .map(f => readFileSync(f, "utf8"))
    .join("\n\n---\n\n");

  // Agents that support skill sub-directories: install each file separately
  const multiFileTargets: { name: string; dir: string }[] = [
    { name: "GitHub Copilot CLI", dir: join(homedir(), ".copilot", "skills", "imi") },
    { name: "Claude Code",        dir: join(homedir(), ".claude",  "skills", "imi") },
  ];

  // Agents that use a single flat file: install concatenated content
  const singleFileTargets: { name: string; dir: string; filename: string }[] = [
    { name: "Cursor",           dir: join(homedir(), ".cursor",   "rules"),          filename: "imi.md"          },
    { name: "OpenCode",         dir: join(homedir(), ".opencode", "instructions"),  filename: "imi-session.md"  },
    { name: "OpenAI Codex",     dir: join(homedir(), ".codex"),                     filename: "instructions.md" },
  ];

  const installed: string[] = [];
  const skipped: string[] = [];

  for (const { name, dir } of multiFileTargets) {
    const agentRoot = join(dir, "..", "..");
    if (!existsSync(agentRoot)) { skipped.push(name); continue; }
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "SKILL.md"), readFileSync(skillSrc, "utf8"));
    for (const sub of subFiles) {
      const src = join(skillsDir, sub);
      if (existsSync(src)) writeFileSync(join(dir, sub), readFileSync(src, "utf8"));
    }
    installed.push(name);
  }

  for (const { name, dir, filename } of singleFileTargets) {
    const agentRoot = join(dir, "..", "..");
    if (!existsSync(agentRoot)) { skipped.push(name); continue; }
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, filename), allContent);
    installed.push(name);
  }

  if (installed.length > 0) {
    console.log(`\nAgent skills installed into: ${installed.join(", ")}`);
    console.log(`Agents will now automatically run imi commands when you mention "imi".`);
  }

  // Also write AGENTS.md and CLAUDE.md in the current working directory if a
  // .imi/ folder exists — keeps project-level agent instructions in sync
  const cwd = process.cwd();
  if (existsSync(join(cwd, ".imi"))) {
    writeFileSync(join(cwd, "AGENTS.md"), allContent);
    writeFileSync(join(cwd, "CLAUDE.md"), allContent);

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
        { stdio: "pipe" }
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
  console.error(`Manual install: curl -fsSL https://aibyimi.com/install | bash`);
  process.exit(1);
});
