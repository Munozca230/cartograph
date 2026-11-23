package com.acme.api;

import com.acme.lib.Money;
import com.acme.lib.UserDto;

public class UserService {
    private final ReportClient reports = new ReportClient();

    public UserDto find(long id) {
        UserDto u = new UserDto();
        u.id = id;
        u.balance = new Money(0);
        return u;
    }

    public UserDto save(UserDto user) {
        return user;
    }

    public void remove(long id) {
    }
}
