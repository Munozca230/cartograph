package com.acme.items.dto;

import lombok.Data;
import org.springframework.hateoas.RepresentationModel;

@Data
public class ExternalDto extends RepresentationModel<ExternalDto> {
    private String title;
}
