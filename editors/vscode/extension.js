const path = require("path");
const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let languageClient;
let watcher;
let transition = Promise.resolve();
let deactivating = false;
let runTerminal;
let replTerminal;
const busyTerminals = new Set();

function ieumCommand() {
  return vscode.workspace.getConfiguration("ieum").get("server.path")?.trim() || "ieum";
}

async function startClient(context, output, status) {
  const command = ieumCommand();
  status.severity = vscode.LanguageStatusSeverity.Information;
  status.text = "이음 서버 시작 중";
  status.busy = true;
  watcher = vscode.workspace.createFileSystemWatcher("**/*.이음");
  const next = new LanguageClient(
    "ieum",
    "이음",
    { command, args: ["lsp"] },
    {
      documentSelector: [
        { scheme: "file", language: "ieum" },
        { scheme: "untitled", language: "ieum" },
      ],
      synchronize: { fileEvents: watcher },
      outputChannel: output,
    },
  );
  languageClient = next;
  try {
    await next.start();
    const version = next.initializeResult?.serverInfo?.version;
    status.text = version ? `이음 ${version}` : "이음";
    status.detail = command;
  } catch (error) {
    languageClient = undefined;
    watcher.dispose();
    output.error(String(error));
    status.severity = vscode.LanguageStatusSeverity.Error;
    status.text = "이음 서버 없음";
    status.detail = `${command}를 실행하지 못했습니다`;
    if (!deactivating) {
      void reportStartFailure(context, output, command);
    }
  } finally {
    status.busy = false;
  }
}

async function reportStartFailure(context, output, command) {
  const choice = await vscode.window.showErrorMessage(
    `이음 언어 서버(${command})를 실행하지 못했습니다. 설치 상태와 ieum.server.path를 확인하세요.`,
    "출력 보기",
    "설정 열기",
    "설치 방법",
  );
  if (choice === "출력 보기") {
    output.show();
  } else if (choice === "설정 열기") {
    vscode.commands.executeCommand("workbench.action.openSettings", "ieum.server.path");
  } else if (choice === "설치 방법") {
    vscode.commands.executeCommand(
      "markdown.showPreview",
      vscode.Uri.file(path.join(context.extensionPath, "README.md")),
    );
  }
}

async function stopClient() {
  const previous = languageClient;
  if (previous) {
    languageClient = undefined;
    await previous.dispose();
    watcher.dispose();
  }
}

function quoted(executable, args) {
  const shell = path.basename(vscode.env.shell || "").toLowerCase();
  if (shell.startsWith("pwsh") || shell.startsWith("powershell")) {
    return `& ${[executable, ...args].map((part) => `'${part.replace(/'/g, "''")}'`).join(" ")}`;
  }
  if (shell.startsWith("cmd")) {
    return [executable, ...args].map((part) => `"${part}"`).join(" ");
  }
  return [executable, ...args].map((part) => `'${part.replace(/'/g, "'\\''")}'`).join(" ");
}

function shellIntegration(terminal, fresh) {
  if (terminal.shellIntegration || !fresh) {
    return Promise.resolve(terminal.shellIntegration);
  }
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      listener.dispose();
      resolve(terminal.shellIntegration);
    }, 3000);
    const listener = vscode.window.onDidChangeTerminalShellIntegration((event) => {
      if (event.terminal === terminal) {
        clearTimeout(timer);
        listener.dispose();
        resolve(event.shellIntegration);
      }
    });
  });
}

async function run() {
  const document = vscode.window.activeTextEditor?.document;
  if (!document || document.languageId !== "ieum") {
    vscode.window.showWarningMessage("실행할 .이음 파일을 열어 주세요.");
    return;
  }
  if (document.uri.scheme !== "file") {
    vscode.window.showWarningMessage("실행하려면 먼저 파일로 저장하세요.");
    return;
  }
  if (document.isDirty && !(await document.save())) {
    return;
  }
  const file = document.uri.fsPath;
  const fresh = !runTerminal || runTerminal.exitStatus !== undefined || busyTerminals.has(runTerminal);
  if (fresh) {
    const folder = vscode.workspace.getWorkspaceFolder(document.uri);
    runTerminal = vscode.window.createTerminal({
      name: "이음",
      cwd: folder ? folder.uri.fsPath : path.dirname(file),
      iconPath: new vscode.ThemeIcon("play"),
    });
  }
  const terminal = runTerminal;
  terminal.show();
  const integration = await shellIntegration(terminal, fresh);
  if (integration) {
    integration.executeCommand(ieumCommand(), [file]);
  } else {
    terminal.sendText(quoted(ieumCommand(), [file]));
  }
}

function repl() {
  if (!replTerminal || replTerminal.exitStatus !== undefined) {
    const folder = vscode.workspace.workspaceFolders?.[0];
    replTerminal = vscode.window.createTerminal({
      name: "이음 대화형",
      shellPath: ieumCommand(),
      shellArgs: ["repl"],
      cwd: folder?.uri.fsPath,
    });
  }
  replTerminal.show();
}

function restart(context, output, status) {
  transition = transition
    .then(async () => {
      await stopClient();
      if (!deactivating) {
        await startClient(context, output, status);
      }
    })
    .catch((error) => output.error(String(error)));
  return transition;
}

function activate(context) {
  deactivating = false;
  const output = vscode.window.createOutputChannel("이음", { log: true });
  const status = vscode.languages.createLanguageStatusItem("ieum.server", { language: "ieum" });
  status.name = "이음 언어 서버";
  status.command = { title: "출력 보기", command: "ieum.showOutput" };
  context.subscriptions.push(
    output,
    status,
    vscode.commands.registerCommand("ieum.run", run),
    vscode.commands.registerCommand("ieum.repl", repl),
    vscode.commands.registerCommand("ieum.restartServer", () => restart(context, output, status)),
    vscode.commands.registerCommand("ieum.showOutput", () => output.show()),
    vscode.window.onDidStartTerminalShellExecution((event) => busyTerminals.add(event.terminal)),
    vscode.window.onDidEndTerminalShellExecution((event) => busyTerminals.delete(event.terminal)),
    vscode.window.onDidCloseTerminal((terminal) => busyTerminals.delete(terminal)),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration("ieum.server.path")) {
        restart(context, output, status);
      }
    }),
  );
  return restart(context, output, status);
}

function deactivate() {
  deactivating = true;
  return transition.then(stopClient);
}

module.exports = { activate, deactivate };
