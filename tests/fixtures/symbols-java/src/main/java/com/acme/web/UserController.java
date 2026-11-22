package com.acme.web;

import com.acme.core.Repo;

@RestController
@RequestMapping("/users")
public class UserController {
    private final Repo repo = new Repo();

    @GetMapping("/{id}")
    public String get(String id) {
        repo.persistEntity();
        return id;
    }

    @PostMapping
    public void create() {}
}
