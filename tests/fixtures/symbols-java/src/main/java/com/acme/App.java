package com.acme;

import com.acme.core.Repo;

public class App {
    public static void main(String[] args) {
        Repo repo = new Repo();
        repo.persistEntity();
        Util.format("x");
        helper();
        System.out.println(repo);
    }

    private static void helper() {}
}
