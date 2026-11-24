package com.acme.items.dto;

import java.util.List;
import lombok.Data;

@Data
public class UpdateItemDto {
    private String projectId;
    private String title;
    private String summary;
    private List<String> tags;
}
