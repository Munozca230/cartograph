const mod = require('./cjs-mod.cjs');
const named = require(moduleName);
const express = require('express');
import('./lazy.mjs').then((m) => m.default);
import(`./locale/${lang}.js`);
const tpl = require(`./cjs-mod.cjs`);
module.exports = { mod, named, express, tpl };
