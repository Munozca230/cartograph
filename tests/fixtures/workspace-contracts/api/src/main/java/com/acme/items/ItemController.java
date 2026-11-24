package com.acme.items;

import com.acme.items.dto.CreateItemDto;
import com.acme.items.dto.ExternalDto;
import com.acme.items.dto.NoteDto;
import com.acme.items.dto.TagDto;
import com.acme.items.dto.UpdateItemDto;
import jakarta.validation.Valid;
import java.util.List;
import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api/items")
public class ItemController {
    @PostMapping
    public String create(@Valid @RequestBody CreateItemDto dto) {
        return "created";
    }

    @PatchMapping("/{id}")
    public String update(@PathVariable String id, @Valid @RequestBody UpdateItemDto dto) {
        return "updated";
    }

    @PostMapping("/batch")
    public String createMany(@Valid @RequestBody List<CreateItemDto> items) {
        return "many";
    }

    // Not validated: its @NotBlank fields are not required.
    @PutMapping("/notes")
    public String note(@RequestBody NoteDto note) {
        return "note";
    }

    @PostMapping("/tags")
    public String tag(@Valid @RequestBody TagDto tag) {
        return "tag";
    }

    // A superclass outside the repo: the shape is not known.
    @PostMapping("/external")
    public String external(@Valid @RequestBody ExternalDto dto) {
        return "external";
    }

    @PostMapping("/raw")
    public String raw(@RequestBody String body) {
        return body;
    }
}
