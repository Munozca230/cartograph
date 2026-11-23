package com.acme.api;

import com.acme.lib.Routes;
import org.springframework.web.bind.annotation.*;

// Routes under a constant of another member: Routes.USERS = Routes.BASE + "/users".
@RestController
@RequestMapping(Routes.USERS)
public class UserController {
    @GetMapping("/{id}")
    public String get(@PathVariable long id) {
        return "user";
    }

    @GetMapping(value = "/me")
    public String me() {
        return "me";
    }

    @PostMapping
    public String create(@RequestBody String body) {
        return body;
    }

    @RequestMapping(value = {"/search", "/find"}, method = RequestMethod.GET)
    public String search(@RequestParam String q) {
        return q;
    }

    // Never called by any member: an unused route.
    @DeleteMapping("/{id}")
    public void delete(@PathVariable long id) {
    }
}
