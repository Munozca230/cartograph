package com.shop.api.notify;

import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;

// The bean is known by the interface only.
@Configuration
public class PrinterConfig {
    @Bean
    public Printer printer() {
        return new LaserPrinter();
    }
}
