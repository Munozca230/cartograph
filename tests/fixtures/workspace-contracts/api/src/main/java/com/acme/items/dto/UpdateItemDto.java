package com.acme.items.dto;

import java.util.List;
import lombok.Data;

@Data
public class UpdateItemDto {
    private String ownerId;
    private String label;
    private String notes;
    private List<String> tags;
}
