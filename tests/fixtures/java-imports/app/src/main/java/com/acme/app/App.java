package com.acme.app;

import com.acme.core.Model;
import com.acme.core.Helper;
import com.acme.core.util.*;
import com.acme.core.util.Where;
import com.acme.core.Missing;
import com.acme.generated.QModel;
import com.fasterxml.jackson.databind.ObjectMapper;
import javax.swing.JFrame;
import javax.persistence.Entity;
import java.util.concurrent.*;
import com.acme.dup.Thing;
import lombok.Data;
import com.acme.core.Bin;

public class App {
    public static void main(String[] args) { new Model(); }
}
