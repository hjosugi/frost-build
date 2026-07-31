package com.example.greeting;

/** The library half of the workspace: no main, packaged as a plain jar. */
public final class Greeting {
    private Greeting() {}

    public static String text() {
        return "frost";
    }
}
