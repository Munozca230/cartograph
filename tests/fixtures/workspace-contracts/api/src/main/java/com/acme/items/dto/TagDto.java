package com.acme.items.dto;

import com.fasterxml.jackson.annotation.JsonProperty;
import jakarta.validation.constraints.NotNull;

public record TagDto(@NotNull String name, @JsonProperty("hex") String color) {}
