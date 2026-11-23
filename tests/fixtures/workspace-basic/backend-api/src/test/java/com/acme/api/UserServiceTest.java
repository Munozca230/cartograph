package com.acme.api;

import com.acme.lib.UserDto;

class UserServiceTest {
    void findsAUser() {
        UserDto u = new UserService().find(1);
        assert u.id == 1;
    }
}
