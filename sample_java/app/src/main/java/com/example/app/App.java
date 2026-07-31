package com.example.app;

import com.example.greeting.Greeting;

/** The application half: compiled against the greeting module's jar. */
public final class App {
    private App() {}

    public static int answer() {
        return 42;
    }

    public static void main(String[] args) {
        System.out.println(Greeting.text() + ": " + answer());
    }
}
