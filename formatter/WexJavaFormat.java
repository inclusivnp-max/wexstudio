import com.google.common.collect.Range;
import com.google.googlejavaformat.java.Formatter;
import com.google.googlejavaformat.java.JavaFormatterOptions;
import java.io.DataOutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.List;

/** Private tool bridge. Source only enters stdin; no user classes are compiled or loaded. */
public final class WexJavaFormat {
    public static void main(String[] args) {
        try {
            if (args.length != 2) throw new IllegalArgumentException("expected UTF16 start/end");
            byte[] bytes = System.in.readNBytes(8 * 1024 * 1024 + 1);
            if (bytes.length > 8 * 1024 * 1024) throw new IllegalArgumentException("source exceeds8MiB");
            String source = StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString();
            int start = Integer.parseInt(args[0]);
            int end = Integer.parseInt(args[1]);
            if (start < 0 || end < start || end > source.length()) throw new IllegalArgumentException("invalid range");
            for (int pos : new int[] {start, end}) {
                if (pos > 0 && pos < source.length() && Character.isHighSurrogate(source.charAt(pos - 1))
                        && Character.isLowSurrogate(source.charAt(pos))) throw new IllegalArgumentException("range splits surrogate");
            }
            var options = JavaFormatterOptions.builder().style(JavaFormatterOptions.Style.AOSP)
                    .formatJavadoc(false).reorderModifiers(false).build();
            // Library replacements, NOT formatSourceAndFixImports: no import deletion/reordering.
            var edits = new Formatter(options).getFormatReplacements(source, List.of(Range.closedOpen(start, end)));
            if (edits.size() > 100000) throw new IllegalArgumentException("too many edits");
            int total = 0;
            for (var edit : edits) {
                total = Math.addExact(total, edit.getReplacementString().getBytes(StandardCharsets.UTF_8).length);
                if (total > 8 * 1024 * 1024) throw new IllegalArgumentException("replacement exceeds8MiB");
            }
            var out = new DataOutputStream(System.out);
            out.writeBytes("WEXJF001");
            out.writeInt(edits.size());
            for (var edit : edits) {
                var range = edit.getReplaceRange();
                byte[] text = edit.getReplacementString().getBytes(StandardCharsets.UTF_8);
                out.writeInt(range.lowerEndpoint()); out.writeInt(range.upperEndpoint());
                out.writeInt(text.length); out.write(text);
            }
            out.flush();
        } catch (Exception | LinkageError error) {
            String message = error.getMessage();
            if (message == null) message = error.getClass().getSimpleName();
            System.err.println("Java formatter: " + message.substring(0, Math.min(message.length(), 4096)));
            System.exit(2);
        }
    }
}
