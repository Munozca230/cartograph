package com.acme.rules;

import jakarta.validation.Constraint;

// The annotation names its validator, which names the annotation: both in one package,
// without an import.
@Constraint(validatedBy = ValidNameValidator.class)
public @interface ValidName {}
