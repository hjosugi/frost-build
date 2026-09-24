// The smallest persistent TypeScript worker that answers #145's question, and
// no more: a measurement instrument, not a Frost feature.
//
//   node ts_worker.mjs <typescript package dir> <fresh|reuse-content|reuse-mtime>
//
// Protocol, one JSON request per line on stdin: {"project": "<tsconfig.json>"}.
// One JSON response per line on stdout:
//   {"exit": 0|1, "compile_ns": N, "cpu_ns": N, "diagnostics": N, "reused_source_files": N}
// cpu_ns is the whole process's user+system time for the request, the number
// comparable with a cold tsc's rusage. The first line written is
// {"ready": true, ...}, so the harness can time Node plus the compiler load
// separately from the first compile.
//
// Every mode checks every file and emits every output on every request, into
// an output directory the harness has emptied: the same contract as a cold
// `tsc -p`, so outputs are comparable byte for byte.
//
// Modes:
//   fresh          a new compiler host and Program per request. What survives is
//                  only the loaded, JIT-compiled compiler.
//   reuse-content  the `tsc --watch` shape: parsed SourceFiles (every lib*.d.ts
//                  included) are kept, versioned by a SHA-256 of their text,
//                  and TypeScript's own incremental builder reuses semantic
//                  diagnostics of files its dependency graph says are
//                  unaffected.
//   reuse-mtime    identical, except that a file's version is its size and
//                  mtime rather than its content. Cheaper, and exactly the
//                  shortcut that goes stale when the content changes but the
//                  stat does not.
import { createHash } from "node:crypto";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import readline from "node:readline";

const [, , typescriptDir, mode] = process.argv;
if (!typescriptDir || !["fresh", "reuse-content", "reuse-mtime"].includes(mode)) {
  process.stderr.write("usage: ts_worker.mjs <typescript package dir> <fresh|reuse-content|reuse-mtime>\n");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const ts = require(path.resolve(typescriptDir));

const cache = new Map();
let oldBuilder;

function fileVersion(fileName) {
  if (mode === "reuse-mtime") {
    const stat = fs.statSync(fileName);
    return { version: `${stat.size}:${stat.mtimeMs}`, text: undefined };
  }
  const text = fs.readFileSync(fileName, "utf8");
  return { version: createHash("sha256").update(text).digest("hex"), text };
}

function reportDiagnostics(diagnostics) {
  for (const diagnostic of diagnostics) {
    process.stderr.write(`${ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n")}\n`);
  }
  return diagnostics.length;
}

function compile(project) {
  let reused = 0;
  const parsed = ts.getParsedCommandLineOfConfigFile(project, {}, {
    ...ts.sys,
    onUnRecoverableConfigFileDiagnostic(diagnostic) {
      throw new Error(ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n"));
    },
  });
  const host = ts.createCompilerHost(parsed.options);
  if (mode === "fresh") {
    const program = ts.createProgram({
      rootNames: parsed.fileNames,
      options: parsed.options,
      host,
      projectReferences: parsed.projectReferences,
    });
    const emitted = program.emit();
    const count = reportDiagnostics(ts.getPreEmitDiagnostics(program).concat(emitted.diagnostics));
    return { exit: count > 0 || emitted.emitSkipped ? 1 : 0, diagnostics: count, reused_source_files: 0 };
  }

  host.getSourceFile = (fileName, languageVersionOrOptions, onError) => {
    if (!fs.existsSync(fileName)) {
      return undefined;
    }
    // impliedNodeFormat comes from the nearest package.json "type", not from
    // the file: a key without the parse options would reuse an ESM parse for
    // CJS.
    const parse = JSON.stringify(languageVersionOrOptions);
    const { version, text } = fileVersion(fileName);
    const hit = cache.get(fileName);
    if (hit && hit.version === version && hit.parse === parse) {
      reused += 1;
      return hit.sourceFile;
    }
    try {
      const sourceFile = ts.createSourceFile(fileName, text ?? fs.readFileSync(fileName, "utf8"), languageVersionOrOptions, false);
      // The builder compares versions to decide what is affected.
      sourceFile.version = version;
      cache.set(fileName, { version, parse, sourceFile });
      return sourceFile;
    } catch (error) {
      onError?.(String(error));
      return undefined;
    }
  };
  const builder = ts.createEmitAndSemanticDiagnosticsBuilderProgram(
    parsed.fileNames,
    parsed.options,
    host,
    oldBuilder,
    ts.getConfigFileParsingDiagnostics(parsed),
    parsed.projectReferences,
  );
  const program = builder.getProgram();
  const diagnostics = [
    ...builder.getConfigFileParsingDiagnostics(),
    ...builder.getOptionsDiagnostics(),
    ...builder.getGlobalDiagnostics(),
    ...builder.getSyntacticDiagnostics(),
    // Cached per file for everything the builder's graph says is unaffected.
    ...builder.getSemanticDiagnostics(),
  ];
  // Emit every file, not only the affected ones: the harness empties the
  // output directory first, as a fresh action output tree would be.
  const emitted = program.emit();
  const count = reportDiagnostics(diagnostics.concat(emitted.diagnostics));
  oldBuilder = builder;
  return { exit: count > 0 || emitted.emitSkipped ? 1 : 0, diagnostics: count, reused_source_files: reused };
}

process.stdout.write(`${JSON.stringify({ ready: true, version: ts.version })}\n`);
const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  if (!line.trim()) {
    continue;
  }
  const request = JSON.parse(line);
  const cpuStart = process.cpuUsage();
  const start = process.hrtime.bigint();
  let response;
  try {
    response = compile(request.project);
  } catch (error) {
    process.stderr.write(`${error.stack ?? error}\n`);
    response = { exit: 1, diagnostics: 1, reused_source_files: 0 };
  }
  response.compile_ns = Number(process.hrtime.bigint() - start);
  const cpu = process.cpuUsage(cpuStart);
  response.cpu_ns = (cpu.user + cpu.system) * 1000;
  process.stdout.write(`${JSON.stringify(response)}\n`);
}
