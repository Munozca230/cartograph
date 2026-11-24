package com.acme.items.dto;

import jakarta.validation.constraints.NotBlank;
import lombok.Data;

@Data
public abstract class BaseDto {
    private static final long serialVersionUID = 1L;

    @NotBlank
    private String projectId;
}
