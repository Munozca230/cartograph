package com.acme.core;

public class Repo {
    public void persistEntity() {
        validate();
    }

    private void validate() {}

    @Override
    public String toString() {
        return "repo";
    }
}
