package com.acme.api;

import com.acme.lib.UserDto;
import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/users")
public class UserController {
    private final UserService service;

    public UserController(UserService service) {
        this.service = service;
    }

    @GetMapping("/{id}")
    public UserDto get(@PathVariable long id) {
        return service.find(id);
    }

    @GetMapping("/me")
    public UserDto me() {
        return service.find(0);
    }

    @PostMapping
    public UserDto create(@RequestBody UserDto user) {
        return service.save(user);
    }

    @DeleteMapping("/{id}")
    public void delete(@PathVariable long id) {
        service.remove(id);
    }
}
