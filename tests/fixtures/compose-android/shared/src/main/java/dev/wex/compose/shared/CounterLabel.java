package dev.wex.compose.shared;
public final class CounterLabel {
    private CounterLabel() {}
    public static String value(int count) { return "Count: " + count; }
}
