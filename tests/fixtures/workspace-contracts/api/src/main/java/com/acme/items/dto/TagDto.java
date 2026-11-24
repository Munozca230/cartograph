package com.acme.items.dto;

import com.fasterxml.jackson.annotation.JsonProperty;
import jakarta.validation.constraints.NotNull;

// `weight` is a primitive: never missing (Jackson sets 0), so `@NotNull` does not make it
// required.
public record TagDto(@NotNull String name, @JsonProperty("hex") String color, @NotNull int weight) {}
