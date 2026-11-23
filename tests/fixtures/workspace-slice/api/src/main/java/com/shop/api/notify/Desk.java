package com.shop.api.notify;

import org.springframework.stereotype.Service;

@Service
public class Desk {
    private final Notifier notifier;
    private final Printer printer;
    private final Scanner scanner;

    public Desk(Notifier notifier, Printer printer, Scanner scanner) {
        this.notifier = notifier;
        this.printer = printer;
        this.scanner = scanner;
    }

    public void work() {
        notifier.send("done");
        printer.print("done");
        scanner.scan();
    }
}
