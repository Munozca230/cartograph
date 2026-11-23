package com.acme.server.support;

import org.springframework.boot.test.autoconfigure.data.mongo.DataMongoTest;

/** Base of the tests that need a Mongo slice. */
@DataMongoTest
public abstract class MongoSliceTest {}
