package com.acme.server.actions;

import com.acme.core.Action;
import java.util.function.Consumer;

/** An action the server can run: its key and the code that runs it. */
public record ActionBinding(Action action, Consumer<String> run) {}
