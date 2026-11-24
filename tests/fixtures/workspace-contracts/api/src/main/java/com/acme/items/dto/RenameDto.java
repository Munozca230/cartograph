package com.acme.items.dto;

import com.fasterxml.jackson.annotation.JsonProperty;
import jakarta.validation.constraints.NotBlank;

// The accessor names the property (`ref`), not the field: the shape is not read.
public class RenameDto {
    @NotBlank
    private String code;

    @JsonProperty("ref")
    public String getCode() {
        return code;
    }

    @JsonProperty("ref")
    public void setCode(String code) {
        this.code = code;
    }
}
