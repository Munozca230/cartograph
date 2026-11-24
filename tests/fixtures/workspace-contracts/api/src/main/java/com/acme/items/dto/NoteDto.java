package com.acme.items.dto;

import jakarta.validation.constraints.NotBlank;

// No Lombok: a field counts when Jackson can set it.
public class NoteDto {
    @NotBlank
    private String text;

    // No accessor: Jackson never sees it.
    private String hidden;

    public String getText() {
        return text;
    }

    public void setText(String text) {
        this.text = text;
    }
}
