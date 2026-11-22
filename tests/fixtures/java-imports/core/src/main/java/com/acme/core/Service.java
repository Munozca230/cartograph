package com.acme.core;

import com.acme.core.util.Strings;
import java.util.List;
import org.apache.commons.lang3.StringUtils;
import com.google.common.collect.ImmutableList;

public class Service {
    List<String> names() { return ImmutableList.of(Strings.trim(StringUtils.EMPTY)); }
}
