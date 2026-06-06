import { spawn } from "node:child_process";
import os from "node:os";
import path from "node:path";

const args = process.argv.slice(2);
const npmBin = process.platform === "win32" ? "npx.cmd" : "npx";
const targetDir = path.join(os.tmpdir(), "gpt-api-tools-tauri-target");
const env = {
  ...process.env,
  COPYFILE_DISABLE: "1"
};

if (!env.CARGO_TARGET_DIR && !env.GITHUB_ACTIONS) {
  env.CARGO_TARGET_DIR = targetDir;
}

const child = spawn(npmBin, ["tauri", ...args], {
  stdio: "inherit",
  env,
  shell: process.platform === "win32"
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});

child.on("error", (error) => {
  console.error(error);
  process.exit(1);
});
