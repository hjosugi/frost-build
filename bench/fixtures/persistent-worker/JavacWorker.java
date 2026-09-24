import java.io.BufferedReader;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.io.StringWriter;
import java.lang.management.ManagementFactory;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import javax.tools.JavaCompiler;
import javax.tools.JavaFileObject;
import javax.tools.StandardJavaFileManager;
import javax.tools.ToolProvider;

/**
 * The smallest persistent javac worker that answers #145's question, and no
 * more: it is a measurement instrument, not a Frost feature.
 *
 * Protocol, one request per line on stdin: the path of an argument file that
 * holds one javac argument per line. One response per line on stdout:
 * {@code <exit code> <compile nanoseconds> <diagnostic bytes> <process CPU
 * nanoseconds>}. The CPU figure is the whole JVM's, JIT and GC threads
 * included, since the request was read -- the number comparable with a cold
 * javac's rusage. Diagnostics go to stderr so they never corrupt the framing.
 * The first line written is {@code ready}, so the harness can time JVM start
 * separately from the first compile.
 *
 * Two modes:
 *
 * <ul>
 *   <li>default: {@code JavaCompiler.run} per request. Every request gets a
 *       fresh javac {@code Context}; what survives between requests is only
 *       the loaded, JIT-compiled compiler classes. That is the state Bazel's
 *       JavaBuilder worker relies on for its speedup.
 *   <li>{@code --shared-file-manager}: one {@code StandardJavaFileManager} is
 *       reused by every request. It caches opened class-path archives, which
 *       is faster and is exactly the kind of state that can outlive the input
 *       it was read from. The harness uses this mode to test that hazard.
 * </ul>
 */
public final class JavacWorker {
    private JavacWorker() {}

    public static void main(String[] args) throws Exception {
        boolean sharedFileManager = args.length > 0 && args[0].equals("--shared-file-manager");
        JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
        if (compiler == null) {
            throw new IllegalStateException("no system Java compiler: run this worker on a JDK");
        }
        StandardJavaFileManager shared =
                sharedFileManager
                        ? compiler.getStandardFileManager(null, null, StandardCharsets.UTF_8)
                        : null;
        com.sun.management.OperatingSystemMXBean os =
                (com.sun.management.OperatingSystemMXBean) ManagementFactory.getOperatingSystemMXBean();
        PrintStream out = new PrintStream(new FileOutputStream(FileDescriptor.out), true, StandardCharsets.UTF_8);
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        out.println("ready");
        String line;
        while ((line = in.readLine()) != null) {
            if (line.isBlank()) {
                continue;
            }
            long cpuStart = os.getProcessCpuTime();
            List<String> argv = Files.readAllLines(Path.of(line), StandardCharsets.UTF_8);
            long start = System.nanoTime();
            int exit;
            String diagnostics;
            if (shared == null) {
                ByteArrayOutputStream err = new ByteArrayOutputStream();
                exit = compiler.run(null, null, new PrintStream(err, true, StandardCharsets.UTF_8), argv.toArray(String[]::new));
                diagnostics = err.toString(StandardCharsets.UTF_8);
            } else {
                List<String> options = new ArrayList<>();
                List<File> sources = new ArrayList<>();
                for (String argument : argv) {
                    if (argument.endsWith(".java")) {
                        sources.add(new File(argument));
                    } else {
                        options.add(argument);
                    }
                }
                StringWriter err = new StringWriter();
                Iterable<? extends JavaFileObject> units = shared.getJavaFileObjectsFromFiles(sources);
                boolean ok = compiler.getTask(err, shared, null, options, null, units).call();
                exit = ok ? 0 : 1;
                diagnostics = err.toString();
            }
            long elapsed = System.nanoTime() - start;
            if (!diagnostics.isEmpty()) {
                System.err.print(diagnostics);
                System.err.flush();
            }
            long cpu = os.getProcessCpuTime() - cpuStart;
            out.println(exit + " " + elapsed + " " + diagnostics.length() + " " + cpu);
        }
    }
}
