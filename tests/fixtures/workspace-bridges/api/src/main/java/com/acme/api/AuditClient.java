package com.acme.api;

import org.springframework.web.client.RestTemplate;

// A call to a host no member serves: external, never matched or reported.
public class AuditClient {
    private final RestTemplate rest = new RestTemplate();

    public void record(String event) {
        rest.postForObject("https://audit.example.com/events", event, String.class);
    }
}
