package com.acme.api;

import org.springframework.web.client.RestTemplate;

public class ReportClient {
    private final RestTemplate rest = new RestTemplate();

    public String daily() {
        return rest.getForObject("https://reports.example.com/daily", String.class);
    }
}
