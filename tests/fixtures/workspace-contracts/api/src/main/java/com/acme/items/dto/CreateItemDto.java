package com.acme.items.dto;

import com.fasterxml.jackson.annotation.JsonAlias;
import com.fasterxml.jackson.annotation.JsonIgnore;
import com.fasterxml.jackson.annotation.JsonProperty;
import jakarta.validation.constraints.NotBlank;
import lombok.Data;

@Data
public class CreateItemDto extends BaseDto {
    @NotBlank
    private String label;

    private String notes;

    @JsonProperty("kind")
    private String type;

    @JsonIgnore
    private String internal;

    @JsonAlias({"color", "colour"})
    private String shade;

    // A constraint of a validation group: not checked by default.
    @NotBlank(groups = Strict.class)
    private String category;

    public interface Strict {}
}
