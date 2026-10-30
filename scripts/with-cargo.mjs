/**
 * 给 tauri CLI 兜一个 cargo 环境。
 *
 * 起因：本机 rustup 装完后没把 `~/.cargo/bin` 写进 PATH，导致 `tauri dev`
 * 内部调 `cargo metadata` 时报 "program not found"，而报错信息完全看不出缺什么。
 *
 * 做两件事，都只影响本次子进程，不改系统设置：
 * 1. PATH 里找不到 cargo 时，把 rustup 的标准安装目录补进去；
 * 2. 直接解析 `node_modules/@tauri-apps/cli/tauri.js` 启动 CLI，
 *    不依赖 PATH 里的 `tauri.cmd`（顺带绕开 shell 拼接参数的告警）。
 *
 * PATH 正常时第 1 步不介入，CI 上等于直通。
 *
 * 用法：node scripts/with-cargo.mjs <tauri 子命令> [...参数]
 */

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const isWindows = process.platform === "win32";
const cargoExe = isWindows ? "cargo.exe" : "cargo";
const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const env = { ...process.env };

// Windows 上 PATH 的真实键名可能是 `Path`。用字面量 `PATH` 取值会拿到 undefined，
// 拼出来就是一个只有 cargo 的 PATH —— node_modules/.bin 全丢，连 tauri 都找不到。
const pathKey = Object.keys(env).find((key) => key.toUpperCase() === "PATH") ?? "PATH";
const currentPath = env[pathKey] ?? "";

function pathHasCargo(value) {
  return value
    .split(delimiter)
    .filter(Boolean)
    .some((dir) => existsSync(join(dir, cargoExe)));
}

if (!pathHasCargo(currentPath)) {
  const cargoBin = join(homedir(), ".cargo", "bin");
  if (!existsSync(join(cargoBin, cargoExe))) {
    console.error(`[termharbor] cargo not found, and ${cargoBin} does not exist.`);
    console.error("[termharbor] install Rust first: https://rustup.rs");
    process.exit(1);
  }
  env[pathKey] = `${cargoBin}${delimiter}${currentPath}`;
  // 用英文输出：Windows 控制台多为 GBK 代码页，中文会显示成乱码
  console.log(`[termharbor] cargo not on PATH, prepended ${cargoBin} for this process only`);
  console.log("[termharbor] to fix permanently: add that folder to your user Path");
}

const args = process.argv.slice(2);
if (args.length === 0) {
  console.error("usage: node scripts/with-cargo.mjs <tauri-subcommand> [...args]");
  process.exit(1);
}

// 优先用 node 直接跑 CLI 入口，避免依赖 .cmd 与 shell
const cliEntry = join(projectRoot, "node_modules", "@tauri-apps", "cli", "tauri.js");

const child = existsSync(cliEntry)
  ? spawn(process.execPath, [cliEntry, ...args], { stdio: "inherit", env })
  : spawn("tauri", args, { stdio: "inherit", env, shell: isWindows });

child.on("error", (err) => {
  console.error(`[termharbor] failed to start tauri CLI: ${err.message}`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});
